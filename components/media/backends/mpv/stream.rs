/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! The bridge between Servo, which pushes media bytes as they are fetched, and libmpv,
//! which pulls them from its demuxer thread. It knows nothing about libmpv itself.

#![cfg_attr(
    not(test),
    expect(dead_code, reason = "Wired into MpvPlayer by the data path task")
)]

use std::collections::VecDeque;
use std::sync::{Condvar, Mutex};

use log::warn;

/// What a [`StreamBridge::read`] did.
#[derive(Debug, PartialEq)]
pub(crate) enum ReadResult {
    /// This many bytes, at least one, were copied into the buffer.
    Data(usize),
    EndOfStream,
    /// The player is gone: the stream will not return anything anymore.
    Cancelled,
}

/// A byte stream filled by Servo with [`push`](Self::push) and drained by libmpv with
/// [`read`](Self::read), which blocks until there is something to return.
#[derive(Default)]
pub(crate) struct StreamBridge {
    state: Mutex<StreamState>,
    /// Wakes up `read` when it can make progress: new data, end of stream, or cancel.
    changed: Condvar,
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
    end_of_stream: bool,
    /// Set for good when the player is gone.
    cancelled: bool,
}

impl StreamBridge {
    pub(crate) fn new() -> StreamBridge {
        StreamBridge::default()
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
        if state.end_of_stream {
            warn!("MPV backend: dropping data pushed after the end of the stream");
            return;
        }
        state.queued += data.len();
        state.chunks.push_back(data);
        self.changed.notify_all();
    }

    pub(crate) fn end_of_stream(&self) {
        self.state.lock().unwrap().end_of_stream = true;
        self.changed.notify_all();
    }

    pub(crate) fn set_size(&self, size: u64) {
        self.state.lock().unwrap().size = Some(size);
    }

    pub(crate) fn size(&self) -> Option<u64> {
        self.state.lock().unwrap().size
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
        ReadResult::Data(copied)
    }

    /// Fails current and future reads, and frees what is queued.
    pub(crate) fn cancel(&self) {
        let mut state = self.state.lock().unwrap();
        state.cancelled = true;
        state.chunks = VecDeque::new();
        state.offset = 0;
        state.queued = 0;
        self.changed.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::mpsc::{self, RecvTimeoutError};
    use std::thread;
    use std::time::Duration;

    use super::*;

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
        let bridge = StreamBridge::new();
        bridge.push(vec![1, 2, 3]);
        bridge.push(vec![4]);
        bridge.push(vec![5, 6]);
        assert_eq!(read_data(&bridge, 10), [1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn a_read_spans_several_chunks() {
        let bridge = StreamBridge::new();
        bridge.push(vec![1, 2, 3]);
        bridge.push(vec![4, 5]);
        let mut buf = [0; 5];
        assert_eq!(bridge.read(&mut buf), ReadResult::Data(5));
        assert_eq!(buf, [1, 2, 3, 4, 5]);
    }

    #[test]
    fn a_short_read_leaves_the_rest_queued() {
        let bridge = StreamBridge::new();
        bridge.push(vec![1, 2, 3, 4, 5]);
        assert_eq!(read_data(&bridge, 2), [1, 2]);
        assert_eq!(read_data(&bridge, 2), [3, 4]);
        assert_eq!(read_data(&bridge, 2), [5]);
    }

    #[test]
    fn reads_keep_the_queue_and_position_consistent() {
        let bridge = StreamBridge::new();
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
        let bridge = Arc::new(StreamBridge::new());
        let result = read_in_background(&bridge, 4);
        assert_waiting(&result);

        bridge.push(vec![7, 8]);
        let (result, buf) = result.recv_timeout(TIMEOUT).unwrap();
        assert_eq!(result, ReadResult::Data(2));
        assert_eq!(buf[..2], [7, 8]);
    }

    #[test]
    fn the_end_of_the_stream_wakes_up_a_waiting_read() {
        let bridge = Arc::new(StreamBridge::new());
        let result = read_in_background(&bridge, 4);
        assert_waiting(&result);

        bridge.end_of_stream();
        let (result, _) = result.recv_timeout(TIMEOUT).unwrap();
        assert_eq!(result, ReadResult::EndOfStream);
    }

    #[test]
    fn cancel_wakes_up_a_waiting_read() {
        let bridge = Arc::new(StreamBridge::new());
        let result = read_in_background(&bridge, 4);
        assert_waiting(&result);

        bridge.cancel();
        let (result, _) = result.recv_timeout(TIMEOUT).unwrap();
        assert_eq!(result, ReadResult::Cancelled);
    }

    #[test]
    fn queued_data_is_read_before_the_end_of_the_stream() {
        let bridge = StreamBridge::new();
        bridge.push(vec![1, 2]);
        bridge.end_of_stream();
        assert_eq!(read_data(&bridge, 4), [1, 2]);
        let mut buf = [0; 4];
        assert_eq!(bridge.read(&mut buf), ReadResult::EndOfStream);
        assert_eq!(bridge.read(&mut buf), ReadResult::EndOfStream);
    }

    #[test]
    fn empty_pushes_are_ignored() {
        let bridge = Arc::new(StreamBridge::new());
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
        let bridge = StreamBridge::new();
        bridge.push(vec![1]);
        bridge.end_of_stream();
        bridge.push(vec![2]);
        assert_eq!(read_data(&bridge, 4), [1]);
        assert_eq!(bridge.read(&mut [0; 4]), ReadResult::EndOfStream);
    }

    #[test]
    fn cancel_is_final_and_frees_the_queue() {
        let bridge = StreamBridge::new();
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
        let bridge = StreamBridge::new();
        assert_eq!(bridge.size(), None);
        bridge.set_size(1234);
        assert_eq!(bridge.size(), Some(1234));
    }

    #[test]
    fn a_megabyte_streams_through_in_order() {
        const LEN: usize = 1 << 20;
        let expected: Vec<u8> = (0..LEN).map(|i| (i % 251) as u8).collect();
        let bridge = Arc::new(StreamBridge::new());

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
        let bridge = Arc::new(StreamBridge::new());

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
        let bridge = Arc::new(StreamBridge::new());

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
}
