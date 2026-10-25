/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! The bridge between Servo, which pushes media bytes as they are fetched, and libmpv,
//! which pulls them from its demuxer thread. It knows nothing about libmpv itself.

use std::collections::VecDeque;
use std::sync::{Condvar, Mutex};

use log::{debug, warn};

/// Above this many queued bytes, the bridge has enough data.
const HIGH_WATER_MARK: usize = 8 * 1024 * 1024;
/// Once it had enough, the bridge needs data again below this many queued bytes.
const LOW_WATER_MARK: usize = 2 * 1024 * 1024;

/// How the bridge asks the player for data. It is called without the bridge's lock held,
/// and must not keep the bridge alive, or neither would ever be freed.
pub(crate) trait BridgeObserver: Send + Sync {
    /// The queue went down below the low water mark after reaching the high one.
    /// Must not call the bridge.
    fn need_data(&self);
    /// The queue went over the high water mark. Must not call the bridge.
    fn enough_data(&self);
    /// Asks Servo to push the resource from `offset` on, and waits for its answer. Must
    /// give up once `bridge` is cancelled.
    fn seek_data(&self, offset: u64, bridge: &StreamBridge) -> SeekReply;
}

/// Servo's answer to a seek, from [`BridgeObserver::seek_data`].
pub(crate) struct SeekReply {
    /// Whether Servo will push the resource from the new offset.
    pub(crate) data_will_come: bool,
    /// Acknowledges the answer to Servo (its `SeekLock`). The bridge calls this only
    /// once it is ready to accept data from the new offset, so that Servo never resumes
    /// pushing before then, as a GStreamer source does (`set_seek_done` before the ack).
    pub(crate) ack: Box<dyn FnOnce() + Send>,
}

/// What a [`StreamBridge::read`] did.
#[derive(Debug, PartialEq)]
pub(crate) enum ReadResult {
    /// This many bytes, at least one, were copied into the buffer.
    Data(usize),
    EndOfStream,
    /// The player is gone, or a seek failed: the stream will not return anything anymore.
    Cancelled,
}

/// A byte stream filled by Servo with [`push`](Self::push) and drained by libmpv with
/// [`read`](Self::read), which blocks until there is something to return.
pub(crate) struct StreamBridge {
    state: Mutex<StreamState>,
    /// Wakes up `read` when it can make progress: new data, end of stream, or cancel.
    changed: Condvar,
    /// Taken before `state` is released to call `need_data` or `enough_data`, so that
    /// they reach the observer in the order of the changes they report.
    notifying: Mutex<()>,
    observer: Box<dyn BridgeObserver>,
}

/// Everything the lock protects. `queued` is the length of `chunks` minus `offset`, and
/// `position + queued` is the offset of the next byte Servo will push.
#[derive(Default)]
struct StreamState {
    /// Kept as pushed, so that pushing copies nothing.
    chunks: VecDeque<Vec<u8>>,
    /// Bytes of `chunks[0]` that were already read.
    offset: usize,
    queued: usize,
    /// Offset in the resource of the next byte `read` returns.
    position: u64,
    size: Option<u64>,
    seekable: bool,
    /// Whether a seek waits for Servo: what it pushes until then comes from the old offset.
    seeking: bool,
    /// Whether the queue went over the high water mark, and not back below the low one.
    full: bool,
    end_of_stream: bool,
    /// Set for good when the player is gone, or a seek failed.
    cancelled: bool,
}

impl StreamState {
    /// Drops what is queued, freeing its memory.
    fn clear(&mut self) {
        self.chunks = VecDeque::new();
        self.offset = 0;
        self.queued = 0;
        self.full = false;
    }
}

impl StreamBridge {
    pub(crate) fn new(observer: Box<dyn BridgeObserver>) -> StreamBridge {
        StreamBridge {
            state: Mutex::default(),
            changed: Condvar::new(),
            notifying: Mutex::default(),
            observer,
        }
    }

    pub(crate) fn push(&self, data: Vec<u8>) {
        // `read` must never return 0 bytes, which libmpv takes as the end of the file.
        if data.is_empty() {
            return;
        }
        let mut state = self.state.lock().unwrap();
        // Data may still arrive while the player is being destroyed.
        if state.cancelled {
            return;
        }
        if state.seeking {
            debug!("MPV backend: dropping data pushed from before a seek");
            return;
        }
        if state.end_of_stream {
            warn!("MPV backend: dropping data pushed after the end of the stream");
            return;
        }
        state.queued += data.len();
        state.chunks.push_back(data);
        let became_full = !state.full && state.queued > HIGH_WATER_MARK;
        state.full |= became_full;
        self.changed.notify_all();
        let notifying = became_full.then(|| self.notifying.lock().unwrap());
        drop(state);

        if notifying.is_some() {
            self.observer.enough_data();
        }
    }

    pub(crate) fn end_of_stream(&self) {
        let mut state = self.state.lock().unwrap();
        // The end of the fetch that a seek replaces.
        if state.seeking {
            return;
        }
        state.end_of_stream = true;
        self.changed.notify_all();
    }

    pub(crate) fn set_size(&self, size: u64) {
        self.state.lock().unwrap().size = Some(size);
    }

    pub(crate) fn size(&self) -> Option<u64> {
        self.state.lock().unwrap().size
    }

    pub(crate) fn set_seekable(&self, seekable: bool) {
        self.state.lock().unwrap().seekable = seekable;
    }

    /// Waits until something is queued, then copies as much of it as fits into `buf`.
    pub(crate) fn read(&self, buf: &mut [u8]) -> ReadResult {
        // libmpv never asks for 0 bytes (`stream_read_unbuffered` in stream/stream.c).
        debug_assert!(!buf.is_empty(), "reading into an empty buffer");

        let state = self.state.lock().unwrap();
        let mut state = self
            .changed
            .wait_while(state, |state| {
                state.queued == 0 && !state.end_of_stream && !state.cancelled
            })
            .unwrap();
        if state.cancelled {
            return ReadResult::Cancelled;
        }
        if state.queued == 0 {
            return ReadResult::EndOfStream;
        }

        let mut copied = 0;
        while copied < buf.len() {
            let StreamState { chunks, offset, .. } = &mut *state;
            let Some(chunk) = chunks.front() else {
                break;
            };
            let count = (chunk.len() - *offset).min(buf.len() - copied);
            buf[copied..copied + count].copy_from_slice(&chunk[*offset..*offset + count]);
            copied += count;
            *offset += count;
            if *offset == chunk.len() {
                chunks.pop_front();
                *offset = 0;
            }
        }
        state.queued -= copied;
        state.position += copied as u64;
        let needs_data = state.full && state.queued < LOW_WATER_MARK;
        state.full &= !needs_data;
        let notifying = needs_data.then(|| self.notifying.lock().unwrap());
        drop(state);

        if notifying.is_some() {
            self.observer.need_data();
        }
        ReadResult::Data(copied)
    }

    /// Moves the stream to `offset`, asking Servo for the data from there if needed.
    /// Returns the new position, or `None` if the stream cannot get there.
    pub(crate) fn seek(&self, offset: u64) -> Option<u64> {
        let mut state = self.state.lock().unwrap();
        if state.cancelled || !state.seekable {
            return None;
        }
        // libmpv seeks to 0 right after opening a stream, and only treats the stream as
        // seekable if that works (stream_cb.c). Servo already pushes from there.
        if offset == state.position {
            return Some(offset);
        }
        state.clear();
        state.position = offset;
        state.end_of_stream = false;
        state.seeking = true;
        drop(state);

        let reply = self.observer.seek_data(offset, self);

        let mut state = self.state.lock().unwrap();
        // Cleared before acknowledging the answer: see `SeekReply::ack`.
        state.seeking = false;
        if !reply.data_will_come {
            // The queue was dropped and the old fetch may go on: the stream cannot be
            // trusted anymore.
            state.cancelled = true;
            state.clear();
            self.changed.notify_all();
        }
        let cancelled = state.cancelled;
        drop(state);
        (reply.ack)();
        (!cancelled).then_some(offset)
    }

    /// Fails current and future reads, and frees what is queued.
    pub(crate) fn cancel(&self) {
        let mut state = self.state.lock().unwrap();
        state.cancelled = true;
        state.clear();
        self.changed.notify_all();
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.state.lock().unwrap().cancelled
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc::{self, RecvTimeoutError};
    use std::sync::{Arc, OnceLock};
    use std::thread;
    use std::time::Duration;

    use super::*;

    /// What a [`TestObserver`] was told.
    #[derive(Default)]
    struct Calls {
        need_data: AtomicUsize,
        enough_data: AtomicUsize,
        seeks: Mutex<Vec<u64>>,
    }

    type SeekAnswer = Box<dyn Fn(u64, &StreamBridge) -> bool + Send + Sync>;

    struct TestObserver {
        calls: Arc<Calls>,
        seek_answer: SeekAnswer,
    }

    impl BridgeObserver for TestObserver {
        fn need_data(&self) {
            self.calls.need_data.fetch_add(1, Ordering::SeqCst);
        }

        fn enough_data(&self) {
            self.calls.enough_data.fetch_add(1, Ordering::SeqCst);
        }

        fn seek_data(&self, offset: u64, bridge: &StreamBridge) -> SeekReply {
            self.calls.seeks.lock().unwrap().push(offset);
            SeekReply {
                data_will_come: (self.seek_answer)(offset, bridge),
                ack: Box::new(|| {}),
            }
        }
    }

    /// A bridge whose observer records calls, and answers seeks with `seek_answer`.
    fn observed_bridge(
        seek_answer: impl Fn(u64, &StreamBridge) -> bool + Send + Sync + 'static,
    ) -> (StreamBridge, Arc<Calls>) {
        let calls = Arc::new(Calls::default());
        let observer = TestObserver {
            calls: calls.clone(),
            seek_answer: Box::new(seek_answer),
        };
        (StreamBridge::new(Box::new(observer)), calls)
    }

    /// A bridge for tests that do not look at its observer.
    fn bridge() -> StreamBridge {
        observed_bridge(|_, _| true).0
    }

    /// Long enough for a reader that is not blocked to have returned.
    const SETTLE: Duration = Duration::from_millis(50);
    /// Upper bound for a woken up reader to return.
    const TIMEOUT: Duration = Duration::from_secs(5);

    /// Reads into a buffer of `len` bytes, expecting data.
    fn read_data(bridge: &StreamBridge, len: usize) -> Vec<u8> {
        let mut buf = vec![0; len];
        match bridge.read(&mut buf) {
            ReadResult::Data(count) => buf[..count].to_vec(),
            other => panic!("expected data, got {other:?}"),
        }
    }

    /// Runs `read` with a buffer of `len` bytes on another thread, so that a test can
    /// check whether it waits. The result comes back on the returned channel.
    fn read_in_background(
        bridge: &Arc<StreamBridge>,
        len: usize,
    ) -> mpsc::Receiver<(ReadResult, Vec<u8>)> {
        let bridge = bridge.clone();
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let mut buf = vec![0; len];
            let result = bridge.read(&mut buf);
            let _ = sender.send((result, buf));
        });
        receiver
    }

    fn assert_waiting(result: &mpsc::Receiver<(ReadResult, Vec<u8>)>) {
        assert_eq!(
            result.recv_timeout(SETTLE).map(|(result, _)| result),
            Err(RecvTimeoutError::Timeout),
            "read returned instead of waiting"
        );
    }

    /// Reads until the end of the stream, with a buffer of `len` bytes.
    fn read_to_end(bridge: &StreamBridge, len: usize) -> Vec<u8> {
        let mut received = vec![];
        let mut buf = vec![0; len];
        loop {
            match bridge.read(&mut buf) {
                ReadResult::Data(count) => received.extend_from_slice(&buf[..count]),
                ReadResult::EndOfStream => return received,
                ReadResult::Cancelled => panic!("the bridge was cancelled"),
            }
        }
    }

    #[test]
    fn pushed_bytes_are_read_in_order() {
        let bridge = bridge();
        bridge.push(vec![1, 2, 3]);
        bridge.push(vec![4]);
        bridge.push(vec![5, 6]);
        assert_eq!(read_data(&bridge, 10), [1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn a_read_spans_several_chunks() {
        let bridge = bridge();
        bridge.push(vec![1, 2, 3]);
        bridge.push(vec![4, 5]);
        let mut buf = [0; 5];
        assert_eq!(bridge.read(&mut buf), ReadResult::Data(5));
        assert_eq!(buf, [1, 2, 3, 4, 5]);
    }

    #[test]
    fn a_short_read_leaves_the_rest_queued() {
        let bridge = bridge();
        bridge.push(vec![1, 2, 3, 4, 5]);
        assert_eq!(read_data(&bridge, 2), [1, 2]);
        assert_eq!(read_data(&bridge, 2), [3, 4]);
        assert_eq!(read_data(&bridge, 2), [5]);
    }

    #[test]
    fn reads_keep_the_queue_and_position_consistent() {
        let bridge = bridge();
        bridge.push(vec![0; 3]);
        bridge.push(vec![0; 4]);
        read_data(&bridge, 2);
        read_data(&bridge, 3);
        let state = bridge.state.lock().unwrap();
        assert_eq!(state.position, 5);
        assert_eq!(state.queued, 2);
        assert_eq!(state.chunks.len(), 1);
        assert_eq!(state.offset, 2);
        let pending: usize = state.chunks.iter().map(Vec::len).sum();
        assert_eq!(state.queued, pending - state.offset);
    }

    #[test]
    fn a_read_waits_for_data() {
        let bridge = Arc::new(bridge());
        let result = read_in_background(&bridge, 4);
        assert_waiting(&result);

        bridge.push(vec![7, 8]);
        let (result, buf) = result.recv_timeout(TIMEOUT).unwrap();
        assert_eq!(result, ReadResult::Data(2));
        assert_eq!(buf[..2], [7, 8]);
    }

    #[test]
    fn the_end_of_the_stream_wakes_up_a_waiting_read() {
        let bridge = Arc::new(bridge());
        let result = read_in_background(&bridge, 4);
        assert_waiting(&result);

        bridge.end_of_stream();
        let (result, _) = result.recv_timeout(TIMEOUT).unwrap();
        assert_eq!(result, ReadResult::EndOfStream);
    }

    #[test]
    fn cancel_wakes_up_a_waiting_read() {
        let bridge = Arc::new(bridge());
        let result = read_in_background(&bridge, 4);
        assert_waiting(&result);

        bridge.cancel();
        let (result, _) = result.recv_timeout(TIMEOUT).unwrap();
        assert_eq!(result, ReadResult::Cancelled);
    }

    #[test]
    fn queued_data_is_read_before_the_end_of_the_stream() {
        let bridge = bridge();
        bridge.push(vec![1, 2]);
        bridge.end_of_stream();
        assert_eq!(read_data(&bridge, 4), [1, 2]);
        let mut buf = [0; 4];
        assert_eq!(bridge.read(&mut buf), ReadResult::EndOfStream);
        assert_eq!(bridge.read(&mut buf), ReadResult::EndOfStream);
    }

    #[test]
    fn empty_pushes_are_ignored() {
        let bridge = Arc::new(bridge());
        let result = read_in_background(&bridge, 4);
        bridge.push(vec![]);
        assert_waiting(&result);
        assert_eq!(bridge.state.lock().unwrap().queued, 0);
        assert!(bridge.state.lock().unwrap().chunks.is_empty());

        // Unblock the reader thread.
        bridge.cancel();
    }

    #[test]
    fn pushes_after_the_end_of_the_stream_are_ignored() {
        let bridge = bridge();
        bridge.push(vec![1]);
        bridge.end_of_stream();
        bridge.push(vec![2]);
        assert_eq!(read_data(&bridge, 4), [1]);
        assert_eq!(bridge.read(&mut [0; 4]), ReadResult::EndOfStream);
    }

    #[test]
    fn cancel_is_final_and_frees_the_queue() {
        let bridge = bridge();
        bridge.push(vec![1, 2, 3]);
        read_data(&bridge, 1);
        bridge.cancel();
        bridge.push(vec![4]);
        bridge.end_of_stream();

        let mut buf = [0; 4];
        assert_eq!(bridge.read(&mut buf), ReadResult::Cancelled);
        assert_eq!(bridge.read(&mut buf), ReadResult::Cancelled);
        let state = bridge.state.lock().unwrap();
        assert!(state.chunks.is_empty());
        assert_eq!(state.chunks.capacity(), 0);
        assert_eq!((state.offset, state.queued), (0, 0));
    }

    #[test]
    fn the_size_is_unknown_until_set() {
        let bridge = bridge();
        assert_eq!(bridge.size(), None);
        bridge.set_size(1234);
        assert_eq!(bridge.size(), Some(1234));
    }

    #[test]
    fn a_megabyte_streams_through_in_order() {
        const LEN: usize = 1 << 20;
        let expected: Vec<u8> = (0..LEN).map(|i| (i % 251) as u8).collect();
        let bridge = Arc::new(bridge());

        let producer = thread::spawn({
            let bridge = bridge.clone();
            let data = expected.clone();
            move || {
                // Irregular chunk sizes, unrelated to the reader's buffer size.
                let mut rest = &data[..];
                let mut size = 1;
                while !rest.is_empty() {
                    let (chunk, tail) = rest.split_at(size.min(rest.len()));
                    bridge.push(chunk.to_vec());
                    rest = tail;
                    size = size * 7 % 9001 + 1;
                }
                bridge.end_of_stream();
            }
        });

        let received = read_to_end(&bridge, 1000);
        producer.join().unwrap();
        assert!(received == expected, "bytes were lost or reordered");
    }

    /// A chunk of the flood tests: its producer, then its rank among that producer's
    /// chunks, so that the reader can tell them apart.
    const RECORD_LEN: usize = 9;

    fn record(producer: u8, rank: u64) -> Vec<u8> {
        let mut record = vec![producer];
        record.extend_from_slice(&rank.to_le_bytes());
        record
    }

    #[test]
    fn a_flood_from_several_threads_loses_nothing() {
        const PRODUCERS: u8 = 4;
        const CHUNKS: u64 = 50_000;
        let bridge = Arc::new(bridge());

        let producers: Vec<_> = (0..PRODUCERS)
            .map(|producer| {
                let bridge = bridge.clone();
                thread::spawn(move || {
                    for rank in 0..CHUNKS {
                        bridge.push(record(producer, rank));
                    }
                })
            })
            .collect();
        let closer = thread::spawn({
            let bridge = bridge.clone();
            move || {
                for producer in producers {
                    producer.join().unwrap();
                }
                bridge.end_of_stream();
            }
        });

        // A buffer size that splits records, to check they are rebuilt correctly.
        let received = read_to_end(&bridge, 4096 + 5);
        closer.join().unwrap();

        assert_eq!(
            received.len(),
            RECORD_LEN * (PRODUCERS as usize) * (CHUNKS as usize)
        );
        let mut next_rank = [0; PRODUCERS as usize];
        for record in received.chunks(RECORD_LEN) {
            let producer = record[0] as usize;
            let rank = u64::from_le_bytes(record[1..].try_into().unwrap());
            assert_eq!(
                rank, next_rank[producer],
                "producer {producer} lost or reordered a chunk"
            );
            next_rank[producer] += 1;
        }
        assert_eq!(next_rank, [CHUNKS; PRODUCERS as usize]);
    }

    #[test]
    fn cancel_during_a_flood_stops_everything() {
        const PRODUCERS: u8 = 4;
        let bridge = Arc::new(bridge());

        let producers: Vec<_> = (0..PRODUCERS)
            .map(|producer| {
                let bridge = bridge.clone();
                thread::spawn(move || {
                    let mut rank = 0;
                    while !bridge.state.lock().unwrap().cancelled {
                        bridge.push(record(producer, rank));
                        rank += 1;
                    }
                })
            })
            .collect();
        let reader = thread::spawn({
            let bridge = bridge.clone();
            move || {
                let mut buf = [0; 1000];
                loop {
                    match bridge.read(&mut buf) {
                        ReadResult::Data(_) => {},
                        other => return other,
                    }
                }
            }
        });

        thread::sleep(SETTLE);
        bridge.cancel();
        let (done, finished) = mpsc::channel();
        thread::spawn(move || {
            let result = reader.join().unwrap();
            for producer in producers {
                producer.join().unwrap();
            }
            let _ = done.send(result);
        });
        assert_eq!(
            finished.recv_timeout(TIMEOUT),
            Ok(ReadResult::Cancelled),
            "threads did not stop after cancel"
        );
        assert_eq!(bridge.state.lock().unwrap().queued, 0);
    }

    #[test]
    fn water_marks_are_reported_once_per_crossing() {
        let (bridge, calls) = observed_bridge(|_, _| true);
        let need = || calls.need_data.load(Ordering::SeqCst);
        let enough = || calls.enough_data.load(Ordering::SeqCst);

        bridge.push(vec![0; HIGH_WATER_MARK]);
        assert_eq!(enough(), 0, "reached, not crossed");
        bridge.push(vec![0; 1]);
        bridge.push(vec![0; 1]);
        assert_eq!(enough(), 1);

        // Read down to just above the low water mark, then below it.
        read_data(&bridge, HIGH_WATER_MARK + 2 - LOW_WATER_MARK);
        assert_eq!(need(), 0);
        read_data(&bridge, 1);
        read_data(&bridge, 1);
        assert_eq!(need(), 1);

        // Back above the high water mark: reported again.
        bridge.push(vec![0; HIGH_WATER_MARK]);
        assert_eq!(enough(), 2);
    }

    #[test]
    fn data_is_not_needed_before_the_queue_was_full() {
        let (bridge, calls) = observed_bridge(|_, _| true);
        bridge.push(vec![0; 10]);
        read_data(&bridge, 10);
        assert_eq!(calls.need_data.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn seeking_to_the_current_position_asks_nothing() {
        let (bridge, calls) = observed_bridge(|_, _| panic!("unexpected seek"));
        bridge.set_seekable(true);
        // libmpv's check right after opening.
        assert_eq!(bridge.seek(0), Some(0));
        bridge.push(vec![1, 2, 3, 4]);
        read_data(&bridge, 3);
        assert_eq!(bridge.seek(3), Some(3));
        assert_eq!(read_data(&bridge, 4), [4]);
        assert!(calls.seeks.lock().unwrap().is_empty());
    }

    #[test]
    fn a_stream_that_is_not_seekable_stays_where_it_is() {
        let (bridge, calls) = observed_bridge(|_, _| panic!("unexpected seek"));
        bridge.push(vec![1, 2, 3]);
        // libmpv's check right after opening must fail, for it to read linearly.
        assert_eq!(bridge.seek(0), None);
        assert_eq!(bridge.seek(100), None);
        assert_eq!(read_data(&bridge, 4), [1, 2, 3]);
        assert!(calls.seeks.lock().unwrap().is_empty());
    }

    #[test]
    fn a_seek_asks_servo_for_data_from_the_new_offset() {
        let (bridge, calls) = observed_bridge(|_, bridge| {
            // Pushed by the fetch the seek replaces, before Servo handles the seek.
            bridge.push(vec![9, 9]);
            bridge.end_of_stream();
            true
        });
        bridge.set_seekable(true);
        bridge.push(vec![1, 2, 3, 4]);
        read_data(&bridge, 2);

        assert_eq!(bridge.seek(100), Some(100));
        assert_eq!(*calls.seeks.lock().unwrap(), [100]);
        {
            let state = bridge.state.lock().unwrap();
            assert_eq!(state.position, 100);
            assert_eq!(state.queued, 0, "old and stale data must be dropped");
            assert!(!state.end_of_stream, "the old fetch's end must be dropped");
        }

        bridge.push(vec![100, 101]);
        bridge.end_of_stream();
        assert_eq!(read_data(&bridge, 4), [100, 101]);
        assert_eq!(bridge.read(&mut [0; 4]), ReadResult::EndOfStream);
    }

    #[test]
    fn a_failed_seek_breaks_the_stream() {
        let (bridge, _) = observed_bridge(|_, _| false);
        bridge.set_seekable(true);
        bridge.push(vec![1, 2, 3]);
        assert_eq!(bridge.seek(100), None);
        // Data of the old fetch must not be mistaken for data at the new offset.
        bridge.push(vec![4]);
        assert_eq!(bridge.read(&mut [0; 4]), ReadResult::Cancelled);
        assert_eq!(bridge.state.lock().unwrap().queued, 0);
    }

    #[test]
    fn cancel_ends_a_seek_waiting_for_servo() {
        let (bridge, _) = observed_bridge(|_, bridge| {
            while !bridge.is_cancelled() {
                thread::sleep(Duration::from_millis(1));
            }
            false
        });
        bridge.set_seekable(true);
        let bridge = Arc::new(bridge);

        let (sender, result) = mpsc::channel();
        thread::spawn({
            let bridge = bridge.clone();
            move || {
                let _ = sender.send(bridge.seek(100));
            }
        });
        assert_eq!(result.recv_timeout(SETTLE), Err(RecvTimeoutError::Timeout));
        bridge.cancel();
        assert_eq!(result.recv_timeout(TIMEOUT), Ok(None));
    }

    #[test]
    fn a_cancelled_stream_cannot_seek() {
        let (bridge, calls) = observed_bridge(|_, _| panic!("unexpected seek"));
        bridge.set_seekable(true);
        bridge.cancel();
        assert_eq!(bridge.seek(0), None);
        assert_eq!(bridge.seek(100), None);
        assert!(calls.seeks.lock().unwrap().is_empty());
    }

    #[test]
    fn water_marks_reach_the_observer_in_order() {
        /// Records the calls it gets, and holds `enough_data` until the test lets it go.
        struct SlowObserver {
            calls: Arc<Mutex<Vec<&'static str>>>,
            entered: Mutex<mpsc::Sender<()>>,
            release: Mutex<mpsc::Receiver<()>>,
        }

        impl BridgeObserver for SlowObserver {
            fn need_data(&self) {
                self.calls.lock().unwrap().push("need_data");
            }

            fn enough_data(&self) {
                self.entered.lock().unwrap().send(()).unwrap();
                self.release.lock().unwrap().recv().unwrap();
                self.calls.lock().unwrap().push("enough_data");
            }

            fn seek_data(&self, _: u64, _: &StreamBridge) -> SeekReply {
                unreachable!()
            }
        }

        let calls = Arc::new(Mutex::new(vec![]));
        let (entered_sender, entered) = mpsc::channel();
        let (release, release_receiver) = mpsc::channel();
        let bridge = Arc::new(StreamBridge::new(Box::new(SlowObserver {
            calls: calls.clone(),
            entered: Mutex::new(entered_sender),
            release: Mutex::new(release_receiver),
        })));

        // The producer crosses the high water mark, and is held while it reports it.
        let producer = thread::spawn({
            let bridge = bridge.clone();
            move || bridge.push(vec![0; HIGH_WATER_MARK + 1])
        });
        entered.recv_timeout(TIMEOUT).unwrap();

        // Meanwhile, the reader drains the queue below the low water mark.
        let reader = thread::spawn({
            let bridge = bridge.clone();
            move || read_data(&bridge, HIGH_WATER_MARK + 2 - LOW_WATER_MARK)
        });
        thread::sleep(SETTLE);
        assert!(
            calls.lock().unwrap().is_empty(),
            "need_data overtook enough_data"
        );

        release.send(()).unwrap();
        producer.join().unwrap();
        reader.join().unwrap();
        assert_eq!(*calls.lock().unwrap(), ["enough_data", "need_data"]);
    }

    #[test]
    fn seek_accepts_servos_data_as_soon_as_it_is_acknowledged() {
        // The ack must unblock Servo only once the bridge is ready to accept data from
        // the new offset, so this push must land, not be dropped as data from the seek
        // being replaced. A wrongly-ordered bridge would drop it and leave the read
        // below waiting forever, so this runs the seek on another thread with a timeout
        // instead of hanging the test suite.
        struct PushOnAck(Arc<OnceLock<Arc<StreamBridge>>>);

        impl BridgeObserver for PushOnAck {
            fn need_data(&self) {}
            fn enough_data(&self) {}
            fn seek_data(&self, _offset: u64, _bridge: &StreamBridge) -> SeekReply {
                let bridge = self.0.clone();
                SeekReply {
                    data_will_come: true,
                    // As Servo does: push the new offset's data as soon as it is
                    // acknowledged, which can run before `seek_data` itself returns.
                    ack: Box::new(move || bridge.get().unwrap().push(vec![9])),
                }
            }
        }

        let cell = Arc::new(OnceLock::new());
        let bridge = Arc::new(StreamBridge::new(Box::new(PushOnAck(cell.clone()))));
        let _ = cell.set(bridge.clone());
        bridge.set_seekable(true);

        let result = read_in_background(&bridge, 4);
        assert_eq!(bridge.seek(100), Some(100));
        let (result, buf) = result
            .recv_timeout(TIMEOUT)
            .expect("the pushed data was dropped, so the read never returned");
        assert_eq!(result, ReadResult::Data(1));
        assert_eq!(buf[0], 9);
    }
}
