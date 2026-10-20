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

/// The properties whose changes are turned into `PlayerEvent`s.
const OBSERVED_PROPERTIES: &[&str] = &[
    "time-pos",
    "duration",
    "pause",
    "idle-active",
    "eof-reached",
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
    pub(crate) fn spawn(
        handle: &Arc<MpvHandle>,
        sender: GenericCallback<PlayerEvent>,
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
                move || forward_events(queue, sender, &stopping)
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
) {
    let mut translator = EventTranslator::default();
    loop {
        let event = queue.wait(-1.0);
        if stopping.load(Ordering::Acquire) || event == Event::Shutdown {
            return;
        }
        for player_event in translator.translate(event, queue.handle(), Instant::now()) {
            let _ = sender.send(player_event);
        }
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
}

impl EventTranslator {
    fn translate(&mut self, event: Event, handle: &MpvHandle, now: Instant) -> Vec<PlayerEvent> {
        match event {
            Event::FileLoaded => {
                self.file_loaded = true;
                self.last_position_update = None;
                let mut events = vec![PlayerEvent::MetadataUpdated(metadata(handle))];
                if let Ok(Node::Flag(paused)) = handle.get_node("pause") {
                    events.push(PlayerEvent::StateChanged(playback_state(paused)));
                }
                events
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
            Event::PropertyChange { name, value } => self
                .property_changed(&name, value, now)
                .into_iter()
                .collect(),
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
            ("eof-reached", Node::Flag(true)) => Some(PlayerEvent::EndOfStream),
            _ => None,
        }
    }
}

/// The metadata of the file loaded by `handle`.
fn metadata(handle: &MpvHandle) -> Metadata {
    let string = |name| match handle.get_node(name) {
        Ok(Node::String(value)) => Some(value),
        _ => None,
    };
    let dimension = |name| match handle.get_node(name) {
        Ok(Node::Int64(value)) => u32::try_from(value).unwrap_or(0),
        _ => 0,
    };
    let duration = match handle.get_node("duration") {
        Ok(Node::Double(duration)) => Duration::try_from_secs_f64(duration).ok(),
        _ => None,
    };

    let mut audio_tracks = vec![];
    let mut video_tracks = vec![];
    if let Ok(Node::Array(tracks)) = handle.get_node("track-list") {
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
    }

    Metadata {
        duration,
        width: dimension("width"),
        height: dimension("height"),
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
}
