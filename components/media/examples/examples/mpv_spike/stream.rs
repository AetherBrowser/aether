/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::ffi::c_char;
use std::fs;
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::slice;
use std::sync::atomic::{AtomicU64, Ordering};

pub(crate) static STREAMS_OPENED: AtomicU64 = AtomicU64::new(0);
pub(crate) static BYTES_READ: AtomicU64 = AtomicU64::new(0);

pub(crate) type MemoryStream = Cursor<Vec<u8>>;

pub(crate) fn open(_: &mut (), uri: &str) -> MemoryStream {
    STREAMS_OPENED.fetch_add(1, Ordering::Relaxed);
    let path = uri.split_once("://").map_or(uri, |(_, path)| path);
    let data = fs::read(path).unwrap_or_else(|err| panic!("{path}: {err}"));
    Cursor::new(data)
}

pub(crate) fn close(_: Box<MemoryStream>) {}

pub(crate) fn read(stream: &mut MemoryStream, buf: &mut [c_char]) -> i64 {
    let buf = unsafe { slice::from_raw_parts_mut(buf.as_mut_ptr().cast::<u8>(), buf.len()) };
    match stream.read(buf) {
        Ok(read) => {
            BYTES_READ.fetch_add(read as u64, Ordering::Relaxed);
            read as i64
        },
        Err(_) => -1,
    }
}

pub(crate) fn seek(stream: &mut MemoryStream, offset: i64) -> i64 {
    match stream.seek(SeekFrom::Start(offset as u64)) {
        Ok(position) => position as i64,
        Err(_) => libmpv2::mpv_error::Generic as i64,
    }
}

pub(crate) fn size(stream: &mut MemoryStream) -> i64 {
    stream.get_ref().len() as i64
}
