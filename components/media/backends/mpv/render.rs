/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Renders the video of a player's libmpv instance in memory, frame after frame, and
//! hands the frames to its media element.

#![cfg_attr(
    not(all(test, has_libmpv)),
    expect(dead_code, reason = "Wired into MpvPlayer by the next task")
)]

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex, Weak};
use std::thread::{self, JoinHandle};

use log::warn;
use servo_base::generic_channel::GenericCallback;
use servo_media_player::PlayerEvent;
use servo_media_player::video::{Buffer, VideoFrame, VideoFrameData, VideoFrameRenderer};

use crate::handle::{MpvError, MpvHandle, RenderContext};

/// The display size of the video, written by the thread that reads mpv's events and read
/// by the render thread, which may not read mpv's properties itself (render.h).
#[derive(Default)]
pub(crate) struct VideoSize(AtomicU64);

impl VideoSize {
    pub(crate) fn set(&self, width: u32, height: u32) {
        self.0.store(
            (u64::from(width) << 32) | u64::from(height),
            Ordering::Release,
        );
    }

    /// `None` until mpv knows the size of the video.
    pub(crate) fn get(&self) -> Option<(u32, u32)> {
        let size = self.0.load(Ordering::Acquire);
        let (width, height) = ((size >> 32) as u32, size as u32);
        (width != 0 && height != 0).then_some((width, height))
    }
}

/// Wakes the render thread up. Never blocks, as libmpv's update callback must not: the
/// channel holds at most one wake-up, so those that come before the thread waits again
/// count as one.
fn wake(sender: &SyncSender<()>) {
    let _ = sender.try_send(());
}

/// A thread that renders the video of a libmpv instance and hands the frames to the media
/// element. Stopped and joined on drop.
pub(crate) struct RenderThread {
    wake: SyncSender<()>,
    stopping: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl RenderThread {
    /// Creates the render context of `handle`, which must happen before the media is
    /// loaded, and starts rendering at the size found in `size`.
    pub(crate) fn spawn(
        handle: Arc<MpvHandle>,
        size: Arc<VideoSize>,
        renderer: Weak<Mutex<dyn VideoFrameRenderer>>,
        sender: GenericCallback<PlayerEvent>,
    ) -> Result<RenderThread, MpvError> {
        let (wake_sender, woken) = mpsc::sync_channel(1);
        // libmpv calls the update callback right away, before the thread exists: the
        // channel keeps that wake-up for it.
        let context = RenderContext::new(handle, {
            let wake_sender = wake_sender.clone();
            move || wake(&wake_sender)
        })?;
        let stopping = Arc::new(AtomicBool::new(false));
        let thread = thread::Builder::new()
            .name("MpvRender".to_owned())
            .spawn({
                let stopping = stopping.clone();
                move || render_frames(context, &woken, &stopping, &size, &renderer, &sender)
            })
            .expect("Thread spawning failed");
        Ok(RenderThread {
            wake: wake_sender,
            stopping,
            thread: Some(thread),
        })
    }

    /// Wakes the render thread up, e.g. so that it renders at a new size.
    pub(crate) fn waker(&self) -> impl Fn() + Send + Sync + 'static {
        let sender = self.wake.clone();
        move || wake(&sender)
    }
}

impl Drop for RenderThread {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        wake(&self.wake);
        if let Some(thread) = self.thread.take() &&
            thread.join().is_err()
        {
            warn!("MPV backend: the render thread panicked");
        }
    }
}

/// The pixels of a frame, in BGRA as Servo expects them.
struct FrameBuffer(Arc<Vec<u8>>);

impl Buffer for FrameBuffer {
    fn to_vec(&self) -> Option<VideoFrameData> {
        Some(VideoFrameData::Raw(self.0.clone()))
    }
}

fn render_frames(
    mut context: RenderContext,
    woken: &Receiver<()>,
    stopping: &AtomicBool,
    size: &VideoSize,
    renderer: &Weak<Mutex<dyn VideoFrameRenderer>>,
    sender: &GenericCallback<PlayerEvent>,
) {
    // A frame mpv announced while the size was still unknown is rendered once it is.
    let mut frame_due = false;
    let mut rendered_size = None;
    // The thread keeps a sender, in its render context's update callback: `recv` only
    // returns once woken up.
    while woken.recv().is_ok() && !stopping.load(Ordering::Acquire) {
        frame_due |= context.update();
        let Some((width, height)) = size.get() else {
            continue;
        };
        // When the size changes, the current frame is rendered again at the new size.
        if !frame_due && rendered_size.is_none_or(|rendered| rendered == (width, height)) {
            continue;
        }
        let Some(len) = (width as usize)
            .checked_mul(height as usize)
            .and_then(|pixels| pixels.checked_mul(4))
        else {
            warn!("MPV backend: cannot render video frames of {width}x{height}");
            continue;
        };
        let mut pixels = vec![0; len];
        // Blocks until the frame is due, which keeps the video in sync with the audio.
        if let Err(error) = context.render_bgr0(width, height, &mut pixels) {
            warn!("MPV backend: cannot render a video frame: {error}");
            continue;
        }
        frame_due = false;
        rendered_size = Some((width, height));
        // libmpv leaves the fourth byte of each bgr0 pixel unspecified, while Servo reads
        // it as alpha.
        for pixel in pixels.as_chunks_mut::<4>().0 {
            pixel[3] = u8::MAX;
        }

        // The media element is gone, and with it the need for frames.
        let Some(renderer) = renderer.upgrade() else {
            return;
        };
        let Some(frame) = VideoFrame::new(
            width as i32,
            height as i32,
            Arc::new(FrameBuffer(Arc::new(pixels))),
        ) else {
            continue;
        };
        // Never held while calling into libmpv: render.h forbids the render thread to wait
        // for a thread that may be calling into libmpv, as Servo's script thread does.
        match renderer.lock() {
            Ok(mut renderer) => renderer.render(frame),
            Err(_) => {
                warn!("MPV backend: the video renderer is poisoned, rendering stops");
                return;
            },
        }
        let _ = sender.send(PlayerEvent::VideoFrameUpdated);
    }
}

#[cfg(all(test, has_libmpv))]
mod tests {
    use std::path::Path;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use super::*;

    #[test]
    fn video_sizes_are_unknown_until_set() {
        let size = VideoSize::default();
        assert_eq!(size.get(), None);
        size.set(1920, 1080);
        assert_eq!(size.get(), Some((1920, 1080)));
        size.set(u32::MAX, 1);
        assert_eq!(size.get(), Some((u32::MAX, 1)));
        size.set(0, 1080);
        assert_eq!(size.get(), None);
    }

    #[test]
    fn wake_ups_before_a_wait_count_as_one() {
        let (sender, woken) = mpsc::sync_channel(1);
        wake(&sender);
        wake(&sender);
        assert!(woken.try_recv().is_ok());
        assert!(woken.try_recv().is_err());
    }

    /// Keeps the frames it is given.
    #[derive(Default)]
    struct Frames(Vec<VideoFrame>);

    impl VideoFrameRenderer for Frames {
        fn render(&mut self, frame: VideoFrame) {
            self.0.push(frame);
        }
    }

    /// The render context keeps the instance alive.
    struct Rendering {
        size: Arc<VideoSize>,
        frames: Arc<Mutex<Frames>>,
        events: mpsc::Receiver<PlayerEvent>,
        thread: RenderThread,
    }

    /// Renders the 2×2 green video, at a size that is not known yet.
    fn render_green_video() -> Rendering {
        let handle = Arc::new(
            MpvHandle::new(&[("ao", "null"), ("vo", "libmpv"), ("keep-open", "yes")]).unwrap(),
        );
        let size = Arc::new(VideoSize::default());
        let frames = Arc::new(Mutex::new(Frames::default()));
        let renderer: Arc<Mutex<dyn VideoFrameRenderer>> = frames.clone();
        let (sender, events) = event_sender();
        let thread = RenderThread::spawn(
            handle.clone(),
            size.clone(),
            Arc::downgrade(&renderer),
            sender,
        )
        .unwrap();
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../tests/wpt/tests/media/2x2-green.webm");
        handle
            .command(&["loadfile", path.to_str().unwrap()])
            .unwrap();
        Rendering {
            size,
            frames,
            events,
            thread,
        }
    }

    /// Like Servo's, this callback never blocks the render thread, unlike the bounded
    /// channel of `GenericCallback::new_blocking`.
    fn event_sender() -> (GenericCallback<PlayerEvent>, mpsc::Receiver<PlayerEvent>) {
        let (sender, events) = mpsc::channel();
        let callback = GenericCallback::new(move |event| {
            if let Ok(event) = event {
                let _ = sender.send(event);
            }
        })
        .unwrap();
        (callback, events)
    }

    fn frame_updated(events: &mpsc::Receiver<PlayerEvent>, timeout: Duration) -> bool {
        matches!(
            events.recv_timeout(timeout),
            Ok(PlayerEvent::VideoFrameUpdated)
        )
    }

    #[test]
    fn frames_are_rendered_once_the_size_is_known() {
        let rendering = render_green_video();
        // The video gets decoded, but nothing is rendered without a size.
        thread::sleep(Duration::from_millis(500));
        assert!(rendering.frames.lock().unwrap().0.is_empty());

        rendering.size.set(2, 2);
        rendering.thread.waker()();
        assert!(frame_updated(&rendering.events, Duration::from_secs(5)));

        let frames = rendering.frames.lock().unwrap();
        let frame = &frames.0[0];
        assert_eq!((frame.get_width(), frame.get_height()), (2, 2));
        for pixel in frame.get_data().chunks(4) {
            let [blue, green, red, alpha] = pixel else {
                unreachable!()
            };
            assert!(
                *green > 100 && *blue < 40 && *red < 40 && *alpha == u8::MAX,
                "{pixel:?} is not opaque green"
            );
        }
    }

    #[test]
    fn a_new_size_renders_the_current_frame_again() {
        let rendering = render_green_video();
        rendering.size.set(2, 2);
        rendering.thread.waker()();
        assert!(frame_updated(&rendering.events, Duration::from_secs(5)));

        rendering.size.set(4, 4);
        rendering.thread.waker()();
        // A frame rendered at the old size may still be announced first.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let timeout = deadline.saturating_duration_since(Instant::now());
            assert!(
                frame_updated(&rendering.events, timeout),
                "no frame at the new size"
            );
            let frames = rendering.frames.lock().unwrap();
            let last = frames.0.last().unwrap();
            if (last.get_width(), last.get_height()) == (4, 4) {
                break;
            }
        }
    }

    #[test]
    fn stopping_the_render_thread_does_not_block() {
        let rendering = render_green_video();
        rendering.size.set(2, 2);
        rendering.thread.waker()();
        assert!(frame_updated(&rendering.events, Duration::from_secs(5)));

        let (stopped, done) = mpsc::channel();
        thread::spawn(move || {
            drop(rendering);
            let _ = stopped.send(());
        });
        assert!(
            done.recv_timeout(Duration::from_secs(5)).is_ok(),
            "the render thread did not stop"
        );
    }
}
