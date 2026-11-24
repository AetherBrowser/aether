/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Forwards the events of a player's libmpv instance to its media element, as
//! `PlayerEvent`s.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use log::warn;
use servo_base::generic_channel::GenericCallback;
use servo_media_player::metadata::Metadata;
use servo_media_player::{PlaybackState, PlayerEvent};

use crate::handle::{EndFileReason, Event, EventQueue, MpvError, MpvHandle, Node};
use crate::render::VideoSize;

/// The properties whose changes are turned into `PlayerEvent`s, or, for `video-params`,
/// into the size the video is rendered at.
const OBSERVED_PROPERTIES: &[&str] = &[
    "time-pos",
    "duration",
    "pause",
    "idle-active",
    "eof-reached",
    "video-params",
];

/// mpv reports `time-pos` on every frame: only forward it this often, like the GStreamer
/// backend does.
const POSITION_UPDATE_INTERVAL: Duration = Duration::from_millis(500);

/// A thread that waits for the events of a libmpv instance and forwards them.
///
/// It is the only consumer of the instance's events, and never touches the player: the
/// script thread may hold the player lock while it waits for libmpv.
pub(crate) struct EventThread {
    handle: Arc<MpvHandle>,
    stopping: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl EventThread {
    /// The display size of the video is published in `video_size`, for the render thread,
    /// which may not read mpv's properties itself. `wake_renderer` is called after each
    /// change, so that the next frame is rendered at the new size. It must not block.
    pub(crate) fn spawn(
        handle: &Arc<MpvHandle>,
        sender: GenericCallback<PlayerEvent>,
        video_size: Arc<VideoSize>,
        wake_renderer: impl Fn() + Send + 'static,
    ) -> Result<EventThread, MpvError> {
        let queue = handle
            .events()
            .expect("The event queue of a new libmpv instance is not taken yet");
        for name in OBSERVED_PROPERTIES {
            handle.observe(name)?;
        }
        let stopping = Arc::new(AtomicBool::new(false));
        let thread = thread::Builder::new()
            .name("MpvEvents".to_owned())
            .spawn({
                let stopping = stopping.clone();
                move || forward_events(queue, sender, &stopping, &video_size, &wake_renderer)
            })
            .expect("Thread spawning failed");
        Ok(EventThread {
            handle: handle.clone(),
            stopping,
            thread: Some(thread),
        })
    }
}

impl Drop for EventThread {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        self.handle.wakeup();
        if let Some(thread) = self.thread.take() &&
            thread.join().is_err()
        {
            warn!("MPV backend: the event thread panicked");
        }
    }
}

fn forward_events(
    mut queue: EventQueue,
    sender: GenericCallback<PlayerEvent>,
    stopping: &AtomicBool,
    video_size: &VideoSize,
    wake_renderer: &dyn Fn(),
) {
    let mut translator = EventTranslator::default();
    loop {
        let event = queue.wait(-1.0);
        if stopping.load(Ordering::Acquire) || event == Event::Shutdown {
            return;
        }
        // Without video parameters, e.g. once the file is unloaded, the size is unknown
        // again and the render thread stops rendering.
        if let Event::PropertyChange { name, value } = &event &&
            name == "video-params"
        {
            let (width, height) = display_size(value).unwrap_or((0, 0));
            video_size.set(width, height);
            wake_renderer();
        }
        for player_event in translator.translate(event, queue.handle(), Instant::now()) {
            let _ = sender.send(player_event);
        }
    }
}

/// The display size in `video-params`: the size of the decoded frames, corrected for the
/// aspect ratio of their pixels.
fn display_size(params: &Node) -> Option<(u32, u32)> {
    let dimension = |key| match params.get(key) {
        Some(Node::Int64(value)) => u32::try_from(*value).ok(),
        _ => None,
    };
    Some((dimension("dw")?, dimension("dh")?))
}

/// The display size of the video track mpv plays, or else of the first one, from what the
/// demuxer reports: mpv's `width` and `height` are 0 with `vid=no`, and before the first
/// decoded frame they are the size of the container, without the aspect correction.
fn video_track_display_size(tracks: &[Node]) -> Option<(u32, u32)> {
    let is_video = |track: &&Node| track.get("type").and_then(Node::as_str) == Some("video");
    let track = tracks
        .iter()
        .filter(is_video)
        .find(|track| track.get("selected") == Some(&Node::Flag(true)))
        .or_else(|| tracks.iter().find(is_video))?;
    let dimension = |key| match track.get(key) {
        Some(Node::Int64(value)) => u32::try_from(*value).ok().filter(|value| *value > 0),
        _ => None,
    };
    let pixel_aspect = match track.get("demux-par") {
        Some(Node::Double(ratio)) if ratio.is_finite() && *ratio > 0.0 => *ratio,
        _ => 1.0,
    };
    Some(correct_aspect(
        dimension("demux-w")?,
        dimension("demux-h")?,
        pixel_aspect,
    ))
}

/// Applies a pixel aspect ratio as mpv does for `video-params/dw` and `dh`: wide pixels
/// make the video wider, tall ones make it taller.
fn correct_aspect(width: u32, height: u32, pixel_aspect: f64) -> (u32, u32) {
    if pixel_aspect > 1.0 {
        ((f64::from(width) * pixel_aspect).round() as u32, height)
    } else {
        (width, (f64::from(height) / pixel_aspect).round() as u32)
    }
}

fn playback_state(paused: bool) -> PlaybackState {
    if paused {
        PlaybackState::Paused
    } else {
        PlaybackState::Playing
    }
}

/// Turns libmpv events into `PlayerEvent`s.
#[derive(Default)]
struct EventTranslator {
    /// Whether a file is loaded. Without one, mpv reports its playback properties as
    /// unavailable or at their defaults, which a media element must not see: an unknown
    /// duration, for instance, means a live stream.
    file_loaded: bool,
    /// The last value of `idle-active`, `None` before libmpv reported it.
    idle: Option<bool>,
    last_position_update: Option<Instant>,
    /// Whether `EndOfStream` was sent since playback last reached the end.
    ended: bool,
}

impl EventTranslator {
    fn translate(&mut self, event: Event, handle: &MpvHandle, now: Instant) -> Vec<PlayerEvent> {
        let mut events = self.translate_event(event, handle, now);
        // Servo only ends playback once the last position it got reaches the duration
        // (`ended_playback`), which the position updates, sent every
        // `POSITION_UPDATE_INTERVAL`, may not have done: report the end of the media first.
        if let Some(index) = events
            .iter()
            .position(|event| matches!(event, PlayerEvent::EndOfStream)) &&
            let Ok(Node::Double(duration)) = handle.get_node("duration")
        {
            events.insert(index, PlayerEvent::PositionChanged(duration));
        }
        events
    }

    fn translate_event(
        &mut self,
        event: Event,
        handle: &MpvHandle,
        now: Instant,
    ) -> Vec<PlayerEvent> {
        match event {
            Event::FileLoaded => {
                self.file_loaded = true;
                self.ended = false;
                self.last_position_update = None;
                let mut events = vec![PlayerEvent::MetadataUpdated(metadata(handle))];
                if let Ok(Node::Flag(paused)) = handle.get_node("pause") {
                    events.push(PlayerEvent::StateChanged(playback_state(paused)));
                }
                events
            },
            // libmpv restarts playback once a seek is done, which is how Servo learns it
            // can leave its seeking state. It also does when the file starts: Servo ignores
            // a `SeekDone` while it does not seek.
            Event::PlaybackRestart => {
                if !self.file_loaded {
                    return vec![];
                }
                self.ended = false;
                self.last_position_update = None;
                match handle.get_node("time-pos") {
                    Ok(Node::Double(position)) => vec![PlayerEvent::SeekDone(position)],
                    _ => vec![],
                }
            },
            // With `keep-open=yes`, the end of the file is reported by `eof-reached`.
            Event::EndFile(reason) => {
                self.file_loaded = false;
                match reason {
                    EndFileReason::Error(error) => {
                        vec![PlayerEvent::Error(format!("MPV backend: {error}"))]
                    },
                    EndFileReason::Eof | EndFileReason::Other => vec![],
                }
            },
            Event::PropertyChange { name, value } => {
                // With `keep-open=yes`, libmpv pauses at the end of the file, but does not
                // always report `eof-reached` when it changes along with `pause`, which
                // happens with short media: check it on pause.
                let ended = self.file_loaded &&
                    name == "pause" &&
                    value == Node::Flag(true) &&
                    handle.get_node("eof-reached") == Ok(Node::Flag(true));
                let mut events: Vec<_> = ended
                    .then(|| self.end_of_stream())
                    .flatten()
                    .into_iter()
                    .collect();
                events.extend(self.property_changed(&name, value, now));
                events
            },
            Event::None | Event::Shutdown | Event::Other => vec![],
        }
    }

    fn property_changed(&mut self, name: &str, value: Node, now: Instant) -> Option<PlayerEvent> {
        if name == "idle-active" {
            let Node::Flag(idle) = value else {
                return None;
            };
            let was_idle = self.idle.replace(idle);
            return (idle && was_idle == Some(false))
                .then_some(PlayerEvent::StateChanged(PlaybackState::Stopped));
        }
        if !self.file_loaded {
            return None;
        }
        match (name, value) {
            ("pause", Node::Flag(paused)) => {
                Some(PlayerEvent::StateChanged(playback_state(paused)))
            },
            ("time-pos", Node::Double(position)) => {
                if self
                    .last_position_update
                    .is_some_and(|last| now.duration_since(last) < POSITION_UPDATE_INTERVAL)
                {
                    return None;
                }
                self.last_position_update = Some(now);
                Some(PlayerEvent::PositionChanged(position))
            },
            ("duration", Node::Double(duration)) => Some(PlayerEvent::DurationChanged(
                Duration::try_from_secs_f64(duration).ok(),
            )),
            ("duration", Node::None) => Some(PlayerEvent::DurationChanged(None)),
            ("eof-reached", Node::Flag(true)) => self.end_of_stream(),
            ("eof-reached", Node::Flag(false)) => {
                self.ended = false;
                None
            },
            _ => None,
        }
    }

    /// `EndOfStream`, unless it was already sent for this end.
    fn end_of_stream(&mut self) -> Option<PlayerEvent> {
        (!std::mem::replace(&mut self.ended, true)).then_some(PlayerEvent::EndOfStream)
    }
}

/// The metadata of the file loaded by `handle`.
///
/// Servo only reads the first metadata of a media, while its `readyState` is still
/// `HAVE_NOTHING`: the size of the video must already be right.
fn metadata(handle: &MpvHandle) -> Metadata {
    let string = |name| match handle.get_node(name) {
        Ok(Node::String(value)) => Some(value),
        _ => None,
    };
    let duration = match handle.get_node("duration") {
        Ok(Node::Double(duration)) => Duration::try_from_secs_f64(duration).ok(),
        _ => None,
    };
    let tracks = match handle.get_node("track-list") {
        Ok(Node::Array(tracks)) => tracks,
        _ => vec![],
    };

    let mut audio_tracks = vec![];
    let mut video_tracks = vec![];
    for track in &tracks {
        let name = match track.get("id") {
            Some(Node::Int64(id)) => id.to_string(),
            _ => String::new(),
        };
        match track.get("type").and_then(Node::as_str) {
            Some("audio") => audio_tracks.push(name),
            Some("video") => video_tracks.push(name),
            _ => {},
        }
    }
    let (width, height) = video_track_display_size(&tracks).unwrap_or((0, 0));

    Metadata {
        duration,
        width,
        height,
        format: string("file-format").unwrap_or_default(),
        is_seekable: matches!(handle.get_node("seekable"), Ok(Node::Flag(true))),
        video_tracks,
        audio_tracks,
        is_live: duration.is_none(),
        title: string("metadata/by-key/title"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loaded() -> EventTranslator {
        EventTranslator {
            file_loaded: true,
            ..Default::default()
        }
    }

    fn change(translator: &mut EventTranslator, name: &str, value: Node) -> Option<PlayerEvent> {
        translator.property_changed(name, value, Instant::now())
    }

    #[test]
    fn nothing_is_reported_before_a_file_is_loaded() {
        let mut translator = EventTranslator::default();
        assert!(change(&mut translator, "pause", Node::Flag(true)).is_none());
        assert!(change(&mut translator, "time-pos", Node::Double(1.0)).is_none());
        assert!(change(&mut translator, "duration", Node::None).is_none());
    }

    #[test]
    fn pause_changes_are_playback_states() {
        let mut translator = loaded();
        assert!(matches!(
            change(&mut translator, "pause", Node::Flag(true)),
            Some(PlayerEvent::StateChanged(PlaybackState::Paused))
        ));
        assert!(matches!(
            change(&mut translator, "pause", Node::Flag(false)),
            Some(PlayerEvent::StateChanged(PlaybackState::Playing))
        ));
    }

    #[test]
    fn going_idle_after_playback_is_stopped() {
        let mut translator = EventTranslator::default();
        assert!(change(&mut translator, "idle-active", Node::Flag(true)).is_none());
        assert!(change(&mut translator, "idle-active", Node::Flag(false)).is_none());
        assert!(matches!(
            change(&mut translator, "idle-active", Node::Flag(true)),
            Some(PlayerEvent::StateChanged(PlaybackState::Stopped))
        ));
    }

    #[test]
    fn position_updates_are_throttled() {
        let mut translator = loaded();
        let start = Instant::now();
        let mut position_at = |offset_ms, position| {
            translator.property_changed(
                "time-pos",
                Node::Double(position),
                start + Duration::from_millis(offset_ms),
            )
        };
        assert!(matches!(
            position_at(0, 0.0),
            Some(PlayerEvent::PositionChanged(0.0))
        ));
        assert!(position_at(100, 0.1).is_none());
        assert!(position_at(499, 0.499).is_none());
        assert!(matches!(
            position_at(500, 0.5),
            Some(PlayerEvent::PositionChanged(0.5))
        ));
    }

    #[test]
    fn durations_are_forwarded_once_loaded() {
        let mut translator = loaded();
        assert!(matches!(
            change(&mut translator, "duration", Node::Double(2.5)),
            Some(PlayerEvent::DurationChanged(Some(duration))) if duration == Duration::from_millis(2500)
        ));
        assert!(matches!(
            change(&mut translator, "duration", Node::None),
            Some(PlayerEvent::DurationChanged(None))
        ));
    }

    #[test]
    fn reaching_the_end_is_end_of_stream() {
        let mut translator = loaded();
        assert!(change(&mut translator, "eof-reached", Node::Flag(false)).is_none());
        assert!(matches!(
            change(&mut translator, "eof-reached", Node::Flag(true)),
            Some(PlayerEvent::EndOfStream)
        ));
    }

    #[test]
    fn the_end_is_reported_once() {
        let mut translator = loaded();
        assert!(matches!(
            change(&mut translator, "eof-reached", Node::Flag(true)),
            Some(PlayerEvent::EndOfStream)
        ));
        assert!(translator.end_of_stream().is_none());
        assert!(change(&mut translator, "eof-reached", Node::Flag(true)).is_none());
        // Played again, e.g. after a seek back.
        assert!(change(&mut translator, "eof-reached", Node::Flag(false)).is_none());
        assert!(matches!(
            change(&mut translator, "eof-reached", Node::Flag(true)),
            Some(PlayerEvent::EndOfStream)
        ));
    }

    #[test]
    #[cfg(has_libmpv)]
    fn restarts_before_a_file_is_loaded_are_not_seeks() {
        let handle = MpvHandle::new(&[("ao", "null"), ("vid", "no")]).unwrap();
        let mut translator = EventTranslator::default();
        assert!(
            translator
                .translate(Event::PlaybackRestart, &handle, Instant::now())
                .is_empty()
        );
    }

    #[test]
    #[cfg(has_libmpv)]
    fn end_of_file_reasons_are_mapped() {
        let handle = MpvHandle::new(&[("ao", "null"), ("vid", "no")]).unwrap();
        let end = |reason| {
            let mut translator = loaded();
            let events = translator.translate(Event::EndFile(reason), &handle, Instant::now());
            assert!(!translator.file_loaded);
            events
        };
        assert!(end(EndFileReason::Eof).is_empty());
        assert!(matches!(
            end(EndFileReason::Error("boom".to_owned())).as_slice(),
            [PlayerEvent::Error(error)] if error == "MPV backend: boom"
        ));
        assert!(end(EndFileReason::Other).is_empty());
    }

    fn map(entries: &[(&str, Node)]) -> Node {
        Node::Map(
            entries
                .iter()
                .map(|(key, value)| ((*key).to_owned(), value.clone()))
                .collect(),
        )
    }

    fn video_track(width: i64, height: i64, extra: &[(&str, Node)]) -> Node {
        let mut entries = vec![
            ("type", Node::String("video".to_owned())),
            ("demux-w", Node::Int64(width)),
            ("demux-h", Node::Int64(height)),
        ];
        entries.extend(extra.iter().cloned());
        map(&entries)
    }

    #[test]
    fn pixel_aspect_ratios_are_applied_as_mpv_does() {
        assert_eq!(correct_aspect(400, 300, 1.0), (400, 300));
        // Anamorphic PAL DVD: 720x576 with 16:11 pixels is 1047x576.
        assert_eq!(correct_aspect(720, 576, 16.0 / 11.0), (1047, 576));
        // Tall pixels make the video taller instead of narrower.
        assert_eq!(correct_aspect(400, 300, 0.5), (400, 600));
    }

    #[test]
    fn the_display_size_is_read_from_video_params() {
        let params = map(&[("dw", Node::Int64(1047)), ("dh", Node::Int64(576))]);
        assert_eq!(display_size(&params), Some((1047, 576)));
        assert_eq!(display_size(&Node::None), None);
        assert_eq!(display_size(&map(&[("dw", Node::Int64(-1))])), None);
    }

    #[test]
    fn the_metadata_size_is_the_corrected_size_of_the_selected_video_track() {
        let audio = map(&[("type", Node::String("audio".to_owned()))]);
        let first = video_track(400, 300, &[]);
        let selected = video_track(
            720,
            576,
            &[
                ("selected", Node::Flag(true)),
                ("demux-par", Node::Double(16.0 / 11.0)),
            ],
        );
        assert_eq!(
            video_track_display_size(&[audio.clone(), first.clone(), selected]),
            Some((1047, 576))
        );
        // With `vid=no`, no video track is selected.
        assert_eq!(
            video_track_display_size(&[audio.clone(), first]),
            Some((400, 300))
        );
        assert_eq!(video_track_display_size(&[audio]), None);
        assert_eq!(video_track_display_size(&[video_track(0, 300, &[])]), None);
    }

    #[cfg(has_libmpv)]
    fn wpt_media(name: &str) -> String {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../tests/wpt/tests/media")
            .join(name)
            .to_str()
            .unwrap()
            .to_owned()
    }

    #[test]
    #[cfg(has_libmpv)]
    fn the_first_metadata_has_the_size_of_the_video() {
        // The real options, with `vid=no`: mpv's `width` and `height` are 0 there. A video
        // without audio would not load at all.
        let options: Vec<_> = crate::BASE_OPTIONS
            .iter()
            .chain(crate::PLAYER_OPTIONS)
            .chain(&[("ao", "null")])
            .copied()
            .collect();
        let handle = Arc::new(MpvHandle::new(&options).unwrap());
        // Like Servo, which reads the metadata sent on `FILE_LOADED`. Unbounded, like
        // Servo's: the test stops reading, and a full channel would block the event thread,
        // then dropping it.
        let (events_sender, events) = std::sync::mpsc::channel();
        let sender = GenericCallback::new(move |event| {
            if let Ok(event) = event {
                let _ = events_sender.send(event);
            }
        })
        .unwrap();
        let _events = EventThread::spawn(&handle, sender, Arc::default(), || {}).unwrap();
        handle
            .command(&["loadfile", &wpt_media("A4.webm")])
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let metadata = loop {
            let timeout = deadline.saturating_duration_since(Instant::now());
            match events.recv_timeout(timeout) {
                Ok(PlayerEvent::MetadataUpdated(metadata)) => break metadata,
                Ok(_) => {},
                Err(_) => panic!("timed out waiting for the metadata"),
            }
        };
        assert_eq!((metadata.width, metadata.height), (320, 240));
    }

    #[test]
    #[cfg(has_libmpv)]
    fn the_render_thread_follows_the_size_of_the_video() {
        use std::sync::atomic::AtomicUsize;
        use std::sync::{Mutex, mpsc};

        use malloc_size_of_derive::MallocSizeOf;
        use servo_media_player::video::{VideoFrame, VideoFrameRenderer};

        use crate::render::RenderThread;

        #[derive(MallocSizeOf)]
        struct Discard;

        impl VideoFrameRenderer for Discard {
            fn render(&mut self, _frame: VideoFrame) {}
        }

        let handle = Arc::new(
            MpvHandle::new(&[("ao", "null"), ("vo", "libmpv"), ("keep-open", "yes")]).unwrap(),
        );
        let ignore = || GenericCallback::new(|_| {}).unwrap();
        let size = Arc::new(VideoSize::default());
        let renderer: Arc<Mutex<dyn VideoFrameRenderer>> = Arc::new(Mutex::new(Discard));
        // The render context must exist before the media is loaded.
        let render = RenderThread::spawn(
            handle.clone(),
            size.clone(),
            Arc::downgrade(&renderer),
            ignore(),
        )
        .unwrap();
        // Records each size the render thread is woken up for.
        let (woken, wake_ups) = mpsc::channel();
        let wake_count = Arc::new(AtomicUsize::new(0));
        let _events = EventThread::spawn(&handle, ignore(), size.clone(), {
            let (size, wake_count, waker) = (size.clone(), wake_count.clone(), render.waker());
            move || {
                wake_count.fetch_add(1, Ordering::SeqCst);
                let _ = woken.send(size.get());
                waker();
            }
        })
        .unwrap();

        handle
            .command(&[
                "loadfile",
                &wpt_media("400x300-red-resize-200x150-green.webm"),
            ])
            .unwrap();
        let wait_for = |expected| {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let timeout = deadline.saturating_duration_since(Instant::now());
                match wake_ups.recv_timeout(timeout) {
                    Ok(size) if size == Some(expected) => return,
                    Ok(_) => {},
                    Err(_) => panic!("the render thread was not woken up for {expected:?}"),
                }
            }
        };
        wait_for((400, 300));
        // The video goes from 400x300 to 200x150 after about a second.
        handle.set_flag("pause", false).unwrap();
        wait_for((200, 150));
        assert_eq!(size.get(), Some((200, 150)));
        assert!(wake_count.load(Ordering::SeqCst) >= 2);
    }
}
