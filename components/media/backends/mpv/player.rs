/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::cell::{Cell, OnceCell};
use std::ops::{Deref, Range};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use log::warn;
use servo_base::generic_channel::{self, GenericCallback, GenericReceiver, TryReceiveError};
use servo_media::MediaInstanceError;
use servo_media_player::{Player, PlayerError, PlayerEvent, SeekLock, SeekLockMsg};
use servo_media_streams::registry::MediaStreamId;
use servo_media_traits::{ClientContextId, MediaInstance};

use crate::Instances;
use crate::events::EventThread;
use crate::handle::{MpvError, MpvHandle, Node};
use crate::stream::{BridgeObserver, SeekReply, StreamBridge};

const DEFAULT_PAUSED: bool = true;
const DEFAULT_MUTED: bool = false;
const DEFAULT_VOLUME: f64 = 1.0;
const DEFAULT_PLAYBACK_RATE: f64 = 1.0;

/// How often a seek waiting for Servo checks whether the player is gone.
const SEEK_ANSWER_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// A media player backed by libmpv.
///
/// Its libmpv instance is only created when first needed, usually when Servo pushes the
/// first bytes of the media, so a media element that never loads costs no mpv threads.
/// Operations that are not wired to libmpv yet report a `PlayerError::Backend` instead of
/// silently succeeding.
pub struct MpvPlayer {
    id: usize,
    context_id: ClientContextId,
    /// The backend's instance table. Weak so a player never keeps its backend alive.
    instances: Weak<Mutex<Instances>>,
    /// Options of the libmpv instance, read when it gets created.
    options: Arc<[(&'static str, &'static str)]>,
    /// Where the events of the libmpv instance go, once it exists.
    sender: GenericCallback<PlayerEvent>,
    /// Where Servo pushes the media, read by libmpv as `aether-media://<id>`.
    bridge: Arc<StreamBridge>,
    /// Whether libmpv was asked to load the media from the bridge.
    loading: Cell<bool>,
    /// Whether Servo can fetch the media from any offset, so that it can be seeked.
    seekable: Cell<bool>,
    handle: OnceCell<Instance>,
    /// Whether this player was playing when its page got suspended, so `resume` restarts
    /// it. A media paused by the page stays paused when the page is resumed.
    resume_after_suspend: Cell<bool>,
    /// The requested mute state, applied to libmpv once its instance exists.
    muted: Cell<bool>,
    /// The requested HTML volume, applied to libmpv once its instance exists.
    volume: Cell<f64>,
}

impl MpvPlayer {
    pub(crate) fn new(
        id: usize,
        context_id: ClientContextId,
        instances: Weak<Mutex<Instances>>,
        options: Arc<[(&'static str, &'static str)]>,
        sender: GenericCallback<PlayerEvent>,
    ) -> MpvPlayer {
        // Servo keeps what it fetches until the player asks for it with `NeedData` (its
        // `BufferedDataSource` starts locked), as GStreamer's appsrc does when it starts.
        // Servo starts its fetch right after creating the player, in the same task: this
        // event reaches it once the fetch exists.
        let _ = sender.send(PlayerEvent::NeedData);
        MpvPlayer {
            id,
            context_id,
            instances,
            options,
            bridge: Arc::new(StreamBridge::new(Box::new(Notifier::new(sender.clone())))),
            sender,
            loading: Cell::new(false),
            seekable: Cell::new(false),
            handle: OnceCell::new(),
            resume_after_suspend: Cell::new(false),
            muted: Cell::new(DEFAULT_MUTED),
            volume: Cell::new(DEFAULT_VOLUME),
        }
    }

    /// The libmpv instance of this player, created on first use.
    fn handle(&self) -> Result<&MpvHandle, PlayerError> {
        if let Some(handle) = self.handle.get() {
            return Ok(handle);
        }
        let handle = Arc::new(MpvHandle::new(&self.options).map_err(backend_error)?);
        if self.muted.get() != DEFAULT_MUTED {
            handle
                .set_flag("mute", self.muted.get())
                .map_err(backend_error)?;
        }
        if self.volume.get() != DEFAULT_VOLUME {
            handle
                .set_double("volume", html_to_mpv_volume(self.volume.get()))
                .map_err(backend_error)?;
        }
        handle
            .add_stream_protocol(self.bridge.clone())
            .map_err(backend_error)?;
        let events = EventThread::spawn(&handle, self.sender.clone()).map_err(backend_error)?;
        Ok(self.handle.get_or_init(|| Instance {
            _events: events,
            handle,
        }))
    }
}

/// Turns the requests of the bridge into `PlayerEvent`s for Servo.
struct Notifier {
    sender: GenericCallback<PlayerEvent>,
    /// Sent to Servo with each `SeekData`, to answer it.
    seek_lock: SeekLock,
    /// Servo's answers to seeks. One channel for the whole life of the player, like the
    /// GStreamer backend's `SeekChannel`: `SeekLock::unlock` panics if it is gone.
    seek_answers: Mutex<GenericReceiver<SeekLockMsg>>,
}

impl Notifier {
    fn new(sender: GenericCallback<PlayerEvent>) -> Notifier {
        let (lock_channel, seek_answers) =
            generic_channel::channel().expect("Couldn't create IPC channel");
        Notifier {
            sender,
            seek_lock: SeekLock { lock_channel },
            seek_answers: Mutex::new(seek_answers),
        }
    }
}

impl BridgeObserver for Notifier {
    fn need_data(&self) {
        // Fails only once the media element is gone, which drops this player soon.
        let _ = self.sender.send(PlayerEvent::NeedData);
    }

    fn enough_data(&self) {
        let _ = self.sender.send(PlayerEvent::EnoughData);
    }

    fn seek_data(&self, offset: u64, bridge: &StreamBridge) -> SeekReply {
        let refused = || SeekReply {
            data_will_come: false,
            ack: Box::new(|| {}),
        };
        // libmpv seeks a stream from a single demuxer thread: there is one seek at a time.
        let answers = self.seek_answers.lock().unwrap();
        if self
            .sender
            .send(PlayerEvent::SeekData(offset, self.seek_lock.clone()))
            .is_err()
        {
            return refused();
        }
        // Servo answers once it fetches from `offset`, but drops the request if the media
        // element changed source meanwhile. Wait in slices, so that cancelling the bridge,
        // which the player does when it goes away, frees libmpv's demuxer thread.
        loop {
            match answers.try_recv_timeout(SEEK_ANSWER_POLL_INTERVAL) {
                Ok((data_will_come, ack)) => {
                    // The bridge then fails libmpv's reads, which libmpv takes as the end
                    // of the media: tell Servo it is an error instead.
                    if !data_will_come {
                        let _ = self.sender.send(PlayerEvent::Error(format!(
                            "MPV backend: the media cannot be fetched from byte {offset}"
                        )));
                    }
                    let sender = self.sender.clone();
                    return SeekReply {
                        data_will_come,
                        // Servo waits for this before pushing the data from `offset`.
                        ack: Box::new(move || {
                            let _ = ack.send(());
                            // The seek started a new fetch, whose data Servo keeps until
                            // it is asked for it again.
                            if data_will_come {
                                let _ = sender.send(PlayerEvent::NeedData);
                            }
                        }),
                    };
                },
                Err(TryReceiveError::Empty) if !bridge.is_cancelled() => {},
                Err(_) => return refused(),
            }
        }
    }
}

/// A libmpv instance with the thread that forwards its events.
struct Instance {
    /// Only kept for its `Drop`. Declared first so the thread is stopped and joined
    /// before `handle` is released, which destroys the instance.
    _events: EventThread,
    handle: Arc<MpvHandle>,
}

impl Deref for Instance {
    type Target = MpvHandle;

    fn deref(&self) -> &MpvHandle {
        &self.handle
    }
}

/// mpv applies a cubic curve to its `volume` property (gain = (volume / 100)³), while an
/// HTML volume is a linear gain in [0, 1]. Taking the cube root makes the two match.
fn html_to_mpv_volume(volume: f64) -> f64 {
    100.0 * volume.clamp(0.0, 1.0).cbrt()
}

fn mpv_to_html_volume(volume: f64) -> f64 {
    (volume / 100.0).powi(3)
}

fn backend_error(error: MpvError) -> PlayerError {
    PlayerError::Backend(format!("MPV backend: {error}"))
}

fn not_implemented(operation: &str) -> PlayerError {
    PlayerError::Backend(format!("MPV backend: {operation} is not implemented yet"))
}

impl Player for MpvPlayer {
    fn play(&self) -> Result<(), PlayerError> {
        self.handle()?
            .set_flag("pause", false)
            .map_err(backend_error)?;
        self.resume_after_suspend.set(false);
        Ok(())
    }

    fn pause(&self) -> Result<(), PlayerError> {
        self.handle()?
            .set_flag("pause", true)
            .map_err(backend_error)?;
        self.resume_after_suspend.set(false);
        Ok(())
    }

    fn paused(&self) -> bool {
        // Without a libmpv instance nothing ever played: don't start one just to answer.
        let Some(handle) = self.handle.get() else {
            return DEFAULT_PAUSED;
        };
        handle.get_flag("pause").unwrap_or_else(|error| {
            warn!("MPV backend: cannot read the pause state: {error}");
            DEFAULT_PAUSED
        })
    }

    fn can_resume(&self) -> bool {
        self.resume_after_suspend.get()
    }

    fn stop(&self) -> Result<(), PlayerError> {
        // Without a libmpv instance nothing ever played: there is nothing to stop.
        let Some(handle) = self.handle.get() else {
            return Ok(());
        };
        handle.set_flag("pause", true).map_err(backend_error)?;
        self.resume_after_suspend.set(false);
        // Seeking needs a loaded file. When mpv is idle, pausing is all there is to do.
        if !handle.get_flag("idle-active").map_err(backend_error)? {
            handle
                .command(&["seek", "0", "absolute"])
                .map_err(backend_error)?;
        }
        Ok(())
    }

    fn seek(&self, time: f64) -> Result<(), PlayerError> {
        // Servo only seeks within `seekable`, so once libmpv loaded the media.
        let Some(handle) = self.handle.get() else {
            return Err(PlayerError::Backend(
                "MPV backend: there is no media to seek".to_owned(),
            ));
        };
        // `exact`: Servo ignores a `SeekDone` more than half a second away from `time`,
        // which a seek to the nearest keyframe could be.
        handle
            .command(&["seek", &time.to_string(), "absolute+exact"])
            .map_err(backend_error)
    }

    fn seekable(&self) -> Vec<Range<f64>> {
        // Servo does not seek outside of these ranges: none when the media cannot be
        // fetched from another offset, or its duration is not known yet.
        if !self.seekable.get() {
            return vec![];
        }
        match self.handle.get().map(|handle| handle.get_node("duration")) {
            Some(Ok(Node::Double(duration))) => vec![0.0..duration],
            _ => vec![],
        }
    }

    fn set_mute(&self, muted: bool) -> Result<(), PlayerError> {
        if let Some(handle) = self.handle.get() {
            handle.set_flag("mute", muted).map_err(backend_error)?;
        }
        self.muted.set(muted);
        Ok(())
    }

    fn muted(&self) -> bool {
        match self.handle.get().map(|handle| handle.get_node("mute")) {
            Some(Ok(Node::Flag(muted))) => muted,
            _ => self.muted.get(),
        }
    }

    fn set_volume(&self, volume: f64) -> Result<(), PlayerError> {
        if let Some(handle) = self.handle.get() {
            handle
                .set_double("volume", html_to_mpv_volume(volume))
                .map_err(backend_error)?;
        }
        self.volume.set(volume.clamp(0.0, 1.0));
        Ok(())
    }

    fn volume(&self) -> f64 {
        match self.handle.get().map(|handle| handle.get_node("volume")) {
            Some(Ok(Node::Double(volume))) => mpv_to_html_volume(volume),
            _ => self.volume.get(),
        }
    }

    fn set_input_size(&self, size: u64) -> Result<(), PlayerError> {
        self.bridge.set_size(size);
        Ok(())
    }

    fn set_seekable(&self, seekable: bool) -> Result<(), PlayerError> {
        // Without it, the bridge answers libmpv's seeks with MPV_ERROR_UNSUPPORTED, without
        // asking Servo: libmpv then reads the stream like a pipe.
        self.bridge.set_seekable(seekable);
        self.seekable.set(seekable);
        Ok(())
    }

    fn set_download_buffering_enabled(&self, _enabled: bool) -> Result<(), PlayerError> {
        // mpv manages its own demuxer cache, there is no download buffering to toggle.
        Ok(())
    }

    fn set_playback_rate(&self, _playback_rate: f64) -> Result<(), PlayerError> {
        Err(not_implemented("set_playback_rate"))
    }

    fn playback_rate(&self) -> f64 {
        DEFAULT_PLAYBACK_RATE
    }

    fn push_data(&self, data: Vec<u8>) -> Result<(), PlayerError> {
        self.bridge.push(data);
        // libmpv starts with the first bytes: Servo only calls `play` once it has the
        // metadata, which libmpv only reports once it loaded the media.
        if !self.loading.get() {
            self.handle()?
                .command(&["loadfile", &format!("aether-media://{}", self.id)])
                .map_err(backend_error)?;
            self.loading.set(true);
        }
        Ok(())
    }

    fn end_of_stream(&self) -> Result<(), PlayerError> {
        self.bridge.end_of_stream();
        Ok(())
    }

    fn buffered(&self) -> Vec<Range<f64>> {
        vec![]
    }

    fn set_stream(&self, _stream: &MediaStreamId, _only_stream: bool) -> Result<(), PlayerError> {
        // libmpv cannot consume servo-media streams (getUserMedia, WebRTC).
        Err(PlayerError::SetStreamFailed)
    }

    fn render_use_gl(&self) -> bool {
        false
    }

    fn set_audio_track(&self, _stream_index: i32, _enabled: bool) -> Result<(), PlayerError> {
        Err(not_implemented("set_audio_track"))
    }

    fn set_video_track(&self, _stream_index: i32, _enabled: bool) -> Result<(), PlayerError> {
        Err(not_implemented("set_video_track"))
    }
}

impl MediaInstance for MpvPlayer {
    fn get_id(&self) -> usize {
        self.id
    }

    fn mute(&self, val: bool) -> Result<(), MediaInstanceError> {
        self.set_mute(val).map_err(|_| MediaInstanceError)
    }

    fn suspend(&self) -> Result<(), MediaInstanceError> {
        if self.paused() {
            return Ok(());
        }
        self.pause().map_err(|_| MediaInstanceError)?;
        self.resume_after_suspend.set(true);
        Ok(())
    }

    fn resume(&self) -> Result<(), MediaInstanceError> {
        if !self.can_resume() {
            return Ok(());
        }
        self.play().map_err(|_| MediaInstanceError)
    }
}

impl Drop for MpvPlayer {
    fn drop(&mut self) {
        // libmpv's demuxer may be waiting for data that will never come: wake it up
        // before the event thread and the instance are torn down.
        self.bridge.cancel();

        // The backend never holds the table lock while a player can be dropped (see
        // `MpvBackend::live_instances`), so this cannot deadlock.
        let Some(instances) = self.instances.upgrade() else {
            return;
        };
        let Ok(mut instances) = instances.lock() else {
            return;
        };
        if let Some(players) = instances.get_mut(&self.context_id) {
            players.retain(|(id, _)| *id != self.id);
            if players.is_empty() {
                instances.remove(&self.context_id);
            }
        }
    }
}

#[cfg(all(test, has_libmpv))]
mod tests {
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;
    use std::time::{Duration, Instant};

    use servo_base::generic_channel::GenericReceiver;
    use servo_media_player::PlaybackState;
    use servo_media_player::metadata::Metadata;

    use super::*;

    /// A player whose libmpv instance never opens an audio output.
    fn player_with(options: &[(&'static str, &'static str)]) -> MpvPlayer {
        MpvPlayer::new(
            0,
            ClientContextId::build(1, 1),
            Weak::new(),
            Arc::from(options),
            GenericCallback::new(|_| {}).unwrap(),
        )
    }

    /// The real player options, without an audio output.
    fn options() -> Vec<(&'static str, &'static str)> {
        crate::BASE_OPTIONS
            .iter()
            .chain(crate::PLAYER_OPTIONS)
            .chain(&[("ao", "null")])
            .copied()
            .collect()
    }

    fn player() -> MpvPlayer {
        player_with(&options())
    }

    /// A player whose events are received by the test.
    fn player_with_events() -> (MpvPlayer, GenericReceiver<PlayerEvent>) {
        player_with_events_and_options(&options())
    }

    fn player_with_events_and_options(
        options: &[(&'static str, &'static str)],
    ) -> (MpvPlayer, GenericReceiver<PlayerEvent>) {
        let (sender, receiver) = GenericCallback::new_blocking().unwrap();
        let player = MpvPlayer::new(
            0,
            ClientContextId::build(1, 1),
            Weak::new(),
            Arc::from(options),
            sender,
        );
        (player, receiver)
    }

    fn test_file() -> Vec<u8> {
        std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../../tests/wpt/tests/media/sine440.mp3"),
        )
        .unwrap()
    }

    /// Five minutes of a 440 Hz sine as an 8 kHz mono WAV, about 4.8 MB: much more than
    /// libmpv reads to open it, so that a seek can go past what Servo fetched so far.
    /// WAV, because FFmpeg seeks it by computing the byte offset of the target time, so a
    /// seek always reaches the stream, while it may read an MP3 up to the target instead.
    fn long_test_file() -> Vec<u8> {
        sine_wav(300)
    }

    /// `seconds` of a 440 Hz sine as an 8 kHz mono WAV: 16 kB per second.
    fn sine_wav(seconds: u32) -> Vec<u8> {
        const RATE: u32 = 8000;
        let samples = (0..RATE * seconds).map(|i| {
            let time = f64::from(i) / f64::from(RATE);
            ((time * 440.0 * std::f64::consts::TAU).sin() * 8000.0) as i16
        });
        let data_size = RATE * seconds * 2;
        let mut wav = Vec::with_capacity(44 + data_size as usize);
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data_size).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
        wav.extend_from_slice(&1u16.to_le_bytes()); // Mono
        wav.extend_from_slice(&RATE.to_le_bytes());
        wav.extend_from_slice(&(RATE * 2).to_le_bytes()); // Bytes per second
        wav.extend_from_slice(&2u16.to_le_bytes()); // Bytes per frame
        wav.extend_from_slice(&16u16.to_le_bytes()); // Bits per sample
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data_size.to_le_bytes());
        for sample in samples {
            wav.extend_from_slice(&sample.to_le_bytes());
        }
        wav
    }

    /// Pushes a whole file as Servo does: its size, then chunks of it, then its end.
    fn push_file(player: &MpvPlayer, data: &[u8]) {
        player.set_input_size(data.len() as u64).unwrap();
        for chunk in data.chunks(16 * 1024) {
            player.push_data(chunk.to_vec()).unwrap();
        }
        player.end_of_stream().unwrap();
    }

    /// Skips events until one matches, failing the test after a few seconds.
    fn expect_event(
        receiver: &GenericReceiver<PlayerEvent>,
        what: &str,
        matches: impl Fn(&PlayerEvent) -> bool,
    ) -> PlayerEvent {
        next_matching(
            |timeout| receiver.try_recv_timeout(timeout).ok(),
            what,
            matches,
        )
    }

    fn next_matching(
        recv: impl Fn(Duration) -> Option<PlayerEvent>,
        what: &str,
        matches: impl Fn(&PlayerEvent) -> bool,
    ) -> PlayerEvent {
        next_matching_within(Duration::from_secs(5), recv, what, matches)
    }

    fn next_matching_within(
        timeout: Duration,
        recv: impl Fn(Duration) -> Option<PlayerEvent>,
        what: &str,
        matches: impl Fn(&PlayerEvent) -> bool,
    ) -> PlayerEvent {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let event = recv(remaining).unwrap_or_else(|| panic!("timed out waiting for {what}"));
            if matches(&event) {
                return event;
            }
        }
    }

    /// How a [`FakeServo`] answers `SeekData`.
    #[derive(Clone, Copy)]
    enum SeekAnswer {
        /// Unlock, then fetch the file from the requested offset, like Servo.
        Fetch,
        /// Unlock with a failure.
        Refuse,
        /// Keep the lock without ever answering.
        Never,
    }

    /// Plays Servo's part for a player: fetches its media over a slow network, answers its
    /// `SeekData`, and forwards every event to the test, `SeekData` included once
    /// answered.
    struct FakeServo {
        events: std::sync::mpsc::Receiver<PlayerEvent>,
        answer: Arc<Mutex<SeekAnswer>>,
        metadata: Metadata,
    }

    /// How long a slow network takes to bring each 16 kB chunk.
    const SLOW_NETWORK: Duration = Duration::from_millis(10);

    /// Pushes `data` from `offset` in 16 kB chunks, one every `delay`, as a network would,
    /// until the file ends, the player is gone, or a newer fetch replaced this one.
    fn fetch(
        bridge: Weak<StreamBridge>,
        data: Arc<Vec<u8>>,
        offset: usize,
        fetches: Arc<AtomicUsize>,
        delay: Duration,
    ) {
        let fetch = fetches.fetch_add(1, Ordering::SeqCst) + 1;
        thread::spawn(move || {
            for chunk in data[offset..].chunks(16 * 1024) {
                thread::sleep(delay);
                let Some(bridge) = bridge.upgrade() else {
                    return;
                };
                if fetches.load(Ordering::SeqCst) != fetch {
                    return;
                }
                bridge.push(chunk.to_vec());
            }
            if let Some(bridge) = bridge.upgrade() &&
                fetches.load(Ordering::SeqCst) == fetch
            {
                bridge.end_of_stream();
            }
        });
    }

    impl FakeServo {
        /// Pushes the first bytes of `data` through the player, which starts libmpv, then
        /// fetches the rest. Seeks are answered with [`SeekAnswer::Fetch`], which libmpv
        /// needs to open a seekable stream: it looks at the end of the file first.
        fn load(
            player: &MpvPlayer,
            events: GenericReceiver<PlayerEvent>,
            data: Vec<u8>,
            seekable: bool,
        ) -> FakeServo {
            let data = Arc::new(data);
            // Weak, like Servo's media element: the test must be able to drop the player.
            let bridge = Arc::downgrade(&player.bridge);
            let fetches = Arc::new(AtomicUsize::new(0));
            let answer = Arc::new(Mutex::new(SeekAnswer::Fetch));
            let (forward, forwarded) = std::sync::mpsc::channel();

            player.set_seekable(seekable).unwrap();
            player.set_input_size(data.len() as u64).unwrap();
            let first_push = FIRST_PUSH.min(data.len());
            player.push_data(data[..first_push].to_vec()).unwrap();
            fetch(
                bridge.clone(),
                data.clone(),
                first_push,
                fetches.clone(),
                SLOW_NETWORK,
            );

            let current_answer = answer.clone();
            thread::spawn(move || {
                let mut kept_locks = vec![];
                while let Ok(event) = events.recv() {
                    if let PlayerEvent::SeekData(offset, ref lock) = event {
                        let answer = *current_answer.lock().unwrap();
                        match answer {
                            SeekAnswer::Fetch => {
                                lock.unlock(true);
                                fetch(
                                    bridge.clone(),
                                    data.clone(),
                                    offset as usize,
                                    fetches.clone(),
                                    SLOW_NETWORK,
                                );
                            },
                            SeekAnswer::Refuse => lock.unlock(false),
                            SeekAnswer::Never => kept_locks.push(lock.clone()),
                        }
                    }
                    if forward.send(event).is_err() {
                        return;
                    }
                }
            });
            let PlayerEvent::MetadataUpdated(metadata) = next_matching(
                |timeout| forwarded.recv_timeout(timeout).ok(),
                "the metadata",
                |event| matches!(event, PlayerEvent::MetadataUpdated(_)),
            ) else {
                unreachable!()
            };
            FakeServo {
                events: forwarded,
                answer,
                metadata,
            }
        }

        fn answer_with(&self, answer: SeekAnswer) {
            *self.answer.lock().unwrap() = answer;
        }

        fn expect(&self, what: &str, matches: impl Fn(&PlayerEvent) -> bool) -> PlayerEvent {
            next_matching(
                |timeout| self.events.recv_timeout(timeout).ok(),
                what,
                matches,
            )
        }

        fn expect_seek(&self) -> u64 {
            let PlayerEvent::SeekData(offset, _) =
                self.expect("a seek", |event| matches!(event, PlayerEvent::SeekData(..)))
            else {
                unreachable!()
            };
            offset
        }
    }

    /// What Servo pushes through the player before its fetch goes on.
    const FIRST_PUSH: usize = 16 * 1024;

    /// Loads a long seekable media through a [`FakeServo`], then answers seeks with
    /// `answer`.
    fn load_seekable(answer: SeekAnswer) -> (MpvPlayer, FakeServo) {
        let (player, events) = player_with_events();
        let servo = FakeServo::load(&player, events, long_test_file(), true);
        servo.answer_with(answer);
        (player, servo)
    }

    #[test]
    fn libmpv_is_only_started_when_needed() {
        let player = player();
        assert!(player.handle.get().is_none());
        player.play().unwrap();
        assert!(player.handle.get().is_some());
    }

    #[test]
    fn new_players_start_paused() {
        let player = player();
        let handle = player.handle().unwrap();
        assert_eq!(handle.get_node("pause"), Ok(Node::Flag(true)));
    }

    #[test]
    fn play_unpauses_libmpv() {
        let player = player();
        player.play().unwrap();
        let handle = player.handle.get().unwrap();
        assert_eq!(handle.get_node("pause"), Ok(Node::Flag(false)));
    }

    #[test]
    fn pause_pauses_libmpv() {
        let player = player();
        player.play().unwrap();
        assert!(!player.paused());
        player.pause().unwrap();
        assert!(player.paused());
        let handle = player.handle.get().unwrap();
        assert_eq!(handle.get_node("pause"), Ok(Node::Flag(true)));
    }

    #[test]
    fn paused_does_not_start_libmpv() {
        let player = player();
        assert!(player.paused());
        assert!(player.handle.get().is_none());
    }

    #[test]
    fn suspend_then_resume_restarts_a_playing_media() {
        let player = player();
        player.play().unwrap();
        player.suspend().unwrap();
        assert!(player.paused());
        assert!(player.can_resume());
        player.resume().unwrap();
        assert!(!player.paused());
        assert!(!player.can_resume());
    }

    #[test]
    fn resume_keeps_a_media_paused_by_the_page_paused() {
        let player = player();
        player.play().unwrap();
        player.pause().unwrap();
        player.suspend().unwrap();
        assert!(!player.can_resume());
        player.resume().unwrap();
        assert!(player.paused());
    }

    #[test]
    fn a_page_action_after_suspend_cancels_the_resume() {
        let player = player();
        player.play().unwrap();
        player.suspend().unwrap();
        player.pause().unwrap();
        player.resume().unwrap();
        assert!(player.paused());
    }

    #[test]
    fn suspend_and_resume_do_not_start_libmpv() {
        let player = player();
        player.suspend().unwrap();
        player.resume().unwrap();
        assert!(player.handle.get().is_none());
    }

    /// Waits until `condition` holds, failing the test after a few seconds.
    fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !condition() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn time_pos(handle: &MpvHandle) -> Option<f64> {
        match handle.get_node("time-pos") {
            Ok(Node::Double(seconds)) => Some(seconds),
            _ => None,
        }
    }

    /// Loads a file directly, as the stream bridge will once it exists.
    fn load_test_file(player: &MpvPlayer) -> &MpvHandle {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../tests/wpt/tests/media/sine440.mp3");
        let handle = player.handle().unwrap();
        handle
            .command(&["loadfile", path.to_str().unwrap()])
            .unwrap();
        wait_until("the file to load", || handle.get_node("duration").is_ok());
        handle
    }

    #[test]
    fn stop_without_libmpv_does_not_start_it() {
        let player = player();
        player.stop().unwrap();
        assert!(player.handle.get().is_none());
    }

    #[test]
    fn stop_without_a_file_only_pauses() {
        let player = player();
        player.play().unwrap();
        player.stop().unwrap();
        assert!(player.paused());
    }

    #[test]
    fn stop_pauses_and_rewinds_a_loaded_file() {
        let player = player();
        let handle = load_test_file(&player);
        player.play().unwrap();
        wait_until("playback to advance", || {
            time_pos(handle).is_some_and(|seconds| seconds > 0.2)
        });

        player.stop().unwrap();
        assert!(player.paused());
        assert!(!player.can_resume());
        wait_until("the rewind", || {
            time_pos(handle).is_some_and(|seconds| seconds < 0.05)
        });
    }

    #[test]
    fn muting_does_not_start_libmpv() {
        let player = player();
        player.set_mute(true).unwrap();
        assert!(player.handle.get().is_none());
        assert!(player.muted());
    }

    #[test]
    fn mute_requested_before_start_is_applied_to_libmpv() {
        let player = player();
        player.set_mute(true).unwrap();
        player.play().unwrap();
        let handle = player.handle.get().unwrap();
        assert_eq!(handle.get_node("mute"), Ok(Node::Flag(true)));
    }

    #[test]
    fn set_mute_drives_the_libmpv_mute_property() {
        let player = player();
        player.play().unwrap();
        let handle = player.handle.get().unwrap();

        player.set_mute(true).unwrap();
        assert_eq!(handle.get_node("mute"), Ok(Node::Flag(true)));
        assert!(player.muted());

        player.set_mute(false).unwrap();
        assert_eq!(handle.get_node("mute"), Ok(Node::Flag(false)));
        assert!(!player.muted());
    }

    #[test]
    fn html_volume_is_converted_to_mpv_cubic_volume() {
        assert_eq!(html_to_mpv_volume(0.0), 0.0);
        assert_eq!(html_to_mpv_volume(1.0), 100.0);
        // cbrt(1/8) is 1/2, but libm's cbrt is not correctly rounded.
        assert!((html_to_mpv_volume(0.125) - 50.0).abs() < 1e-12);
        assert_eq!(mpv_to_html_volume(50.0), 0.125);
        assert_eq!(html_to_mpv_volume(-1.0), 0.0);
        assert_eq!(html_to_mpv_volume(2.0), 100.0);
    }

    #[test]
    fn volume_change_does_not_start_libmpv() {
        let player = player();
        player.set_volume(0.125).unwrap();
        assert!(player.handle.get().is_none());
        assert_eq!(player.volume(), 0.125);
    }

    #[test]
    fn volume_requested_before_start_is_applied_to_libmpv() {
        let player = player();
        player.set_volume(0.125).unwrap();
        player.play().unwrap();
        let handle = player.handle.get().unwrap();
        assert_eq!(handle.get_node("volume"), Ok(Node::Double(50.0)));
    }

    #[test]
    fn set_volume_drives_the_libmpv_volume_property() {
        let player = player();
        player.play().unwrap();
        let handle = player.handle.get().unwrap();

        player.set_volume(0.125).unwrap();
        assert_eq!(handle.get_node("volume"), Ok(Node::Double(50.0)));
        assert_eq!(player.volume(), 0.125);

        player.set_volume(0.0).unwrap();
        assert_eq!(handle.get_node("volume"), Ok(Node::Double(0.0)));
        assert_eq!(player.volume(), 0.0);
    }

    #[test]
    fn loading_a_file_reports_its_metadata_then_its_state() {
        let (player, events) = player_with_events();
        load_test_file(&player);
        let PlayerEvent::MetadataUpdated(metadata) =
            expect_event(&events, "the metadata", |event| {
                matches!(event, PlayerEvent::MetadataUpdated(_))
            })
        else {
            unreachable!()
        };
        assert!(metadata.duration.is_some());
        assert!(!metadata.is_live);
        assert!(metadata.is_seekable);
        assert_eq!(metadata.audio_tracks.len(), 1);
        assert!(metadata.video_tracks.is_empty());
        expect_event(&events, "the paused state", |event| {
            matches!(event, PlayerEvent::StateChanged(PlaybackState::Paused))
        });
    }

    #[test]
    fn playback_reports_its_state_and_position() {
        let (player, events) = player_with_events();
        load_test_file(&player);
        expect_event(&events, "the metadata", |event| {
            matches!(event, PlayerEvent::MetadataUpdated(_))
        });
        player.play().unwrap();
        expect_event(&events, "the playing state", |event| {
            matches!(event, PlayerEvent::StateChanged(PlaybackState::Playing))
        });
        expect_event(
            &events,
            "the position to advance",
            |event| matches!(event, PlayerEvent::PositionChanged(position) if *position > 0.0),
        );
    }

    #[test]
    fn reaching_the_end_reports_end_of_stream_then_paused() {
        let (player, events) = player_with_events();
        let handle = load_test_file(&player);
        let Ok(Node::Double(duration)) = handle.get_node("duration") else {
            panic!("the test file should have a duration");
        };
        handle
            .command(&["seek", &(duration - 0.2).to_string(), "absolute"])
            .unwrap();
        player.play().unwrap();
        expect_event(&events, "the end of stream", |event| {
            matches!(event, PlayerEvent::EndOfStream)
        });
        // `keep-open=yes` pauses at the end instead of unloading the file.
        expect_event(&events, "the paused state", |event| {
            matches!(event, PlayerEvent::StateChanged(PlaybackState::Paused))
        });
    }

    #[test]
    fn a_file_that_cannot_be_played_reports_an_error() {
        let (player, events) = player_with_events();
        player
            .handle()
            .unwrap()
            .command(&["loadfile", "/nonexistent/aether-media-mpv-test.mp3"])
            .unwrap();
        expect_event(&events, "the error", |event| {
            matches!(event, PlayerEvent::Error(_))
        });
    }

    #[test]
    fn libmpv_starts_with_the_first_pushed_bytes() {
        let player = player();
        player.set_input_size(1024).unwrap();
        assert!(player.handle.get().is_none());
        player.push_data(test_file()[..1024].to_vec()).unwrap();
        assert!(player.handle.get().is_some());
    }

    #[test]
    fn pushed_media_is_loaded_and_played() {
        let (player, events) = player_with_events();
        push_file(&player, &test_file());
        let PlayerEvent::MetadataUpdated(metadata) =
            expect_event(&events, "the metadata", |event| {
                matches!(event, PlayerEvent::MetadataUpdated(_))
            })
        else {
            unreachable!()
        };
        assert!(metadata.duration.is_some());
        assert_eq!(metadata.audio_tracks.len(), 1);
        player.play().unwrap();
        expect_event(
            &events,
            "the position to advance",
            |event| matches!(event, PlayerEvent::PositionChanged(position) if *position > 0.0),
        );
    }

    #[test]
    fn pushed_media_plays_to_its_end() {
        // Decode as fast as possible instead of in real time.
        let mut options = options();
        options.push(("ao-null-untimed", "yes"));
        let (player, events) = player_with_events_and_options(&options);
        push_file(&player, &test_file());
        let PlayerEvent::MetadataUpdated(metadata) =
            expect_event(&events, "the metadata", |event| {
                matches!(event, PlayerEvent::MetadataUpdated(_))
            })
        else {
            unreachable!()
        };
        let duration = metadata.duration.unwrap().as_secs_f64();
        player.play().unwrap();
        let next = || {
            expect_event(&events, "the end of stream", |event| {
                matches!(
                    event,
                    PlayerEvent::PositionChanged(_) | PlayerEvent::EndOfStream
                )
            })
        };
        let mut position = None;
        while let PlayerEvent::PositionChanged(new_position) = next() {
            position = Some(new_position);
        }
        // Servo only fires `ended` once the position it knows reaches the duration.
        let position = position.expect("a position before the end of stream");
        assert!(
            (position - duration).abs() < 1e-3,
            "the end of stream comes at {position} s, not at the duration ({duration} s)"
        );
    }

    #[test]
    fn a_player_waiting_for_data_is_dropped_without_hanging() {
        let player = player();
        // Not enough for libmpv to open the media: its demuxer waits for more.
        player.push_data(test_file()[..1024].to_vec()).unwrap();
        thread::sleep(Duration::from_millis(200));

        let (dropped, done) = std::sync::mpsc::channel();
        thread::spawn(move || {
            drop(player);
            dropped.send(()).unwrap();
        });
        done.recv_timeout(Duration::from_secs(5))
            .expect("dropping the player should not wait for more data");
    }

    #[test]
    fn water_marks_are_reported_as_player_events() {
        // Unbounded, unlike `new_blocking`: both events are sent before any is received.
        let (events_sender, events) = std::sync::mpsc::channel();
        let sender = GenericCallback::new(move |event| {
            let _ = events_sender.send(event.unwrap());
        })
        .unwrap();
        let notifier = Notifier::new(sender);
        notifier.enough_data();
        notifier.need_data();
        let next = || events.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(matches!(next(), PlayerEvent::EnoughData));
        assert!(matches!(next(), PlayerEvent::NeedData));
    }

    #[test]
    fn seeking_past_the_fetched_data_asks_servo_for_it() {
        let (player, servo) = load_seekable(SeekAnswer::Fetch);
        let handle = player.handle.get().unwrap();
        handle.command(&["seek", "250", "absolute"]).unwrap();
        // 250 s is about 3/4 of the file.
        assert!(servo.expect_seek() > long_test_file().len() as u64 / 2);
        player.play().unwrap();
        wait_until("playback from the new position", || {
            time_pos(handle).is_some_and(|seconds| seconds > 250.0)
        });
    }

    #[test]
    fn a_seek_refused_by_servo_fails_the_media() {
        let (player, servo) = load_seekable(SeekAnswer::Refuse);
        player
            .handle
            .get()
            .unwrap()
            .command(&["seek", "250", "absolute"])
            .unwrap();
        servo.expect_seek();
        servo.expect("the error", |event| matches!(event, PlayerEvent::Error(_)));
    }

    #[test]
    fn a_player_waiting_for_a_seek_is_dropped_without_hanging() {
        let (player, servo) = load_seekable(SeekAnswer::Never);
        player
            .handle
            .get()
            .unwrap()
            .command(&["seek", "250", "absolute"])
            .unwrap();
        servo.expect_seek();

        let (dropped, done) = std::sync::mpsc::channel();
        thread::spawn(move || {
            drop(player);
            dropped.send(()).unwrap();
        });
        done.recv_timeout(Duration::from_secs(5))
            .expect("dropping the player should end the seek");
    }

    #[test]
    fn an_unseekable_stream_never_asks_servo_to_seek() {
        let (player, events) = player_with_events();
        let servo = FakeServo::load(&player, events, long_test_file(), false);
        let handle = player.handle.get().unwrap();
        player.play().unwrap();
        wait_until("playback to advance", || {
            time_pos(handle).is_some_and(|seconds| seconds > 0.5)
        });
        // Neither a seek nor a stop may wait for Servo.
        handle.command(&["seek", "250", "absolute"]).unwrap();
        player.stop().unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        while let Ok(event) = servo
            .events
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        {
            assert!(!matches!(event, PlayerEvent::SeekData(..)));
        }
    }

    #[test]
    fn stop_rewinds_from_the_cache() {
        let (player, servo) = load_seekable(SeekAnswer::Fetch);
        let handle = player.handle.get().unwrap();
        player.play().unwrap();
        wait_until("playback to advance", || {
            time_pos(handle).is_some_and(|seconds| seconds > 1.0)
        });

        player.stop().unwrap();
        wait_until("the rewind", || {
            time_pos(handle).is_some_and(|seconds| seconds < 0.05)
        });
        // The start of the media is still in libmpv's cache: nothing is fetched again.
        while let Ok(event) = servo.events.recv_timeout(Duration::from_millis(500)) {
            assert!(!matches!(event, PlayerEvent::SeekData(..)));
        }
        player.play().unwrap();
        wait_until("playback to start again", || {
            time_pos(handle).is_some_and(|seconds| seconds > 0.2)
        });
    }

    #[test]
    fn a_loaded_seekable_media_can_be_seeked_whole() {
        let (player, _servo) = load_seekable(SeekAnswer::Fetch);
        assert_eq!(player.seekable(), vec![0.0..300.0]);
    }

    #[test]
    fn an_unseekable_media_has_no_seekable_range() {
        let (player, events) = player_with_events();
        push_file(&player, &test_file());
        expect_event(&events, "the metadata", |event| {
            matches!(event, PlayerEvent::MetadataUpdated(_))
        });
        assert!(player.seekable().is_empty());
    }

    #[test]
    fn seeking_without_a_media_fails() {
        assert!(matches!(player().seek(1.0), Err(PlayerError::Backend(_))));
    }

    #[test]
    fn a_media_played_to_its_end_can_be_seeked_back() {
        // What Servo does on `play()` after `ended`: seek to the start, then play.
        let mut options = options();
        options.push(("ao-null-untimed", "yes"));
        let (player, events) = player_with_events_and_options(&options);
        let servo = FakeServo::load(&player, events, test_file(), true);
        player.play().unwrap();
        servo.expect("the end of stream", |event| {
            matches!(event, PlayerEvent::EndOfStream)
        });
        player.seek(0.0).unwrap();
        servo.expect(
            "the seek back to be done",
            |event| matches!(event, PlayerEvent::SeekDone(position) if *position < 0.5),
        );
    }

    #[test]
    fn a_seek_reports_seek_done_at_the_requested_time() {
        let (player, servo) = load_seekable(SeekAnswer::Fetch);
        player.seek(250.0).unwrap();
        // Past what Servo fetched: libmpv needs the data from there first.
        servo.expect_seek();
        servo.expect(
            "the seek to be done",
            |event| matches!(event, PlayerEvent::SeekDone(position) if (position - 250.0).abs() < 0.5),
        );
    }

    #[test]
    fn a_media_larger_than_the_buffers_plays_to_its_end() {
        // 50 minutes, about 48 MB: more than libmpv's cache (32 MiB) and the 8 MB the bridge
        // then holds, past which it asks Servo to stop with `EnoughData`.
        let data = Arc::new(sine_wav(3000));
        let mut options = options();
        // Decode as fast as possible: the media is long enough for libmpv to report its
        // property changes.
        options.push(("ao-null-untimed", "yes"));
        let (player, events) = player_with_events_and_options(&options);
        let player = Arc::new(Mutex::new(player));
        let bridge = Arc::downgrade(&player.lock().unwrap().bridge);
        let fetches = Arc::new(AtomicUsize::new(0));
        let resumed = Arc::new(AtomicUsize::new(0));
        let (ended, end) = std::sync::mpsc::channel();

        // Servo's reactions, from htmlmediaelement.rs: `EnoughData` cancels the fetch, a
        // `NeedData` then seeks to the current position, and `SeekData` fetches again from
        // the requested offset. That seek would be served from libmpv's cache, so the fetch
        // would never resume: the player must resume it with `SeekData`.
        thread::spawn({
            // Weak, like Servo's media element: the test must be able to drop the player.
            let player = Arc::downgrade(&player);
            let (data, bridge, fetches, resumed) = (
                data.clone(),
                bridge.clone(),
                fetches.clone(),
                resumed.clone(),
            );
            move || {
                let mut position = 0.0;
                let mut cancelled = false;
                while let Ok(event) = events.recv() {
                    match event {
                        PlayerEvent::PositionChanged(new_position) => position = new_position,
                        PlayerEvent::EnoughData => {
                            fetches.fetch_add(1, Ordering::SeqCst);
                            cancelled = true;
                        },
                        PlayerEvent::NeedData if cancelled => {
                            cancelled = false;
                            let Some(player) = player.upgrade() else {
                                return;
                            };
                            player.lock().unwrap().seek(position).unwrap();
                        },
                        PlayerEvent::SeekData(offset, lock) => {
                            if std::mem::take(&mut cancelled) {
                                resumed.fetch_add(1, Ordering::SeqCst);
                            }
                            lock.unlock(true);
                            let (bridge, data, fetches) =
                                (bridge.clone(), data.clone(), fetches.clone());
                            fetch(bridge, data, offset as usize, fetches, Duration::ZERO);
                        },
                        PlayerEvent::EndOfStream => {
                            let _ = ended.send(());
                        },
                        _ => {},
                    }
                }
            }
        });

        {
            let player = player.lock().unwrap();
            player.set_seekable(true).unwrap();
            player.set_input_size(data.len() as u64).unwrap();
            player.push_data(data[..FIRST_PUSH].to_vec()).unwrap();
            // A fast network: it fills the bridge before libmpv drains it.
            fetch(bridge, data.clone(), FIRST_PUSH, fetches, Duration::ZERO);
            player.play().unwrap();
        }
        end.recv_timeout(Duration::from_secs(60))
            .expect("playback should reach the end of the media");
        assert!(
            resumed.load(Ordering::SeqCst) > 0,
            "the fetch should have been stopped, then resumed"
        );
    }

    /// Where the format tests find their media.
    const WPT: &str = "../../../../tests/wpt/tests/";

    /// The codec of each track of the loaded media, as `(type, codec)`.
    fn track_codecs(handle: &MpvHandle) -> Vec<(String, String)> {
        let Ok(Node::Array(tracks)) = handle.get_node("track-list") else {
            return vec![];
        };
        tracks
            .iter()
            .map(|track| {
                let string = |key| track.get(key).and_then(Node::as_str).unwrap_or_default();
                (string("type").to_owned(), string("codec").to_owned())
            })
            .collect()
    }

    /// Plays a media to its end through the path Servo uses: `push_data`, the bridge, then
    /// libmpv, which seeks through `SeekData` when it needs to. Video is not decoded yet
    /// (`vid=no`): its track must be recognized, and the audio played.
    ///
    /// In real time, as a page plays it: with `ao-null-untimed`, libmpv plays a short media
    /// within one iteration of its loop, and reports none of its property changes.
    fn plays_to_its_end(path: &str, audio_codec: &str, video_codec: Option<&str>) {
        let data = std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join(path))
            .unwrap_or_else(|error| panic!("cannot read {path}: {error}"));
        let (player, events) = player_with_events();
        let servo = FakeServo::load(&player, events, data, true);

        assert_eq!(servo.metadata.audio_tracks.len(), 1, "{path}");
        assert_eq!(
            servo.metadata.video_tracks.len(),
            usize::from(video_codec.is_some()),
            "{path}"
        );
        let mut expected = vec![("audio".to_owned(), audio_codec.to_owned())];
        expected.extend(video_codec.map(|codec| ("video".to_owned(), codec.to_owned())));
        let mut codecs = track_codecs(player.handle.get().unwrap());
        codecs.sort();
        expected.sort();
        assert_eq!(codecs, expected, "{path}");

        player.play().unwrap();
        let duration = servo.metadata.duration.expect("a duration");
        next_matching_within(
            duration + Duration::from_secs(5),
            |timeout| servo.events.recv_timeout(timeout).ok(),
            &format!("the end of {path}"),
            |event| matches!(event, PlayerEvent::EndOfStream),
        );
    }

    #[test]
    fn mp3_plays() {
        plays_to_its_end(&format!("{WPT}media/sine440.mp3"), "mp3", None);
    }

    #[test]
    fn libmpv_failures_are_reported_as_backend_errors() {
        let player = player_with(&[("not-an-option", "1")]);
        let Err(PlayerError::Backend(message)) = player.play() else {
            panic!("play() should fail when libmpv cannot be created");
        };
        assert!(message.starts_with("MPV backend: mpv_set_option_string(not-an-option=1)"));
    }
}
