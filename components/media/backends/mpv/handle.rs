/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Safe wrapper around an `mpv_handle`. This is the only module allowed to call into `ffi`.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::mem::MaybeUninit;
use std::panic::{self, AssertUnwindSafe};
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::{fmt, slice};

use crate::ffi;
use crate::stream::{ReadResult, StreamBridge};

/// Major version of the client API that `ffi.rs` is written against.
const SUPPORTED_API_MAJOR: u32 = 2;

#[derive(Debug, Clone, PartialEq)]
pub enum MpvError {
    /// A libmpv call returned a negative `mpv_error` code.
    Api { call: String, code: c_int },
    /// `mpv_create()` returned NULL.
    CreateFailed,
    /// The linked libmpv implements a client API this crate was not written for.
    UnsupportedApi { major: u32, minor: u32 },
    /// A string passed to libmpv contains an interior NUL byte.
    InvalidString(String),
    /// A property did not have the expected shape.
    UnexpectedFormat { property: String },
}

impl fmt::Display for MpvError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MpvError::Api { call, code } => {
                write!(formatter, "{call} failed: {}", error_string(*code))
            },
            MpvError::CreateFailed => write!(formatter, "mpv_create() returned NULL"),
            MpvError::UnsupportedApi { major, minor } => write!(
                formatter,
                "libmpv client API {major}.{minor} is not supported (need {SUPPORTED_API_MAJOR}.x)"
            ),
            MpvError::InvalidString(string) => {
                write!(formatter, "string contains a NUL byte: {string:?}")
            },
            MpvError::UnexpectedFormat { property } => {
                write!(formatter, "property {property:?} has an unexpected format")
            },
        }
    }
}

impl std::error::Error for MpvError {}

/// The client API version of the linked libmpv, as `(major, minor)`.
pub fn client_api_version() -> (u32, u32) {
    let version = unsafe { ffi::mpv_client_api_version() };
    (((version >> 16) & 0xffff) as u32, (version & 0xffff) as u32)
}

fn error_string(code: c_int) -> String {
    // mpv_error_string() returns a static string for any input, never NULL.
    unsafe { CStr::from_ptr(ffi::mpv_error_string(code)) }
        .to_string_lossy()
        .into_owned()
}

fn check(code: c_int, call: impl FnOnce() -> String) -> Result<(), MpvError> {
    if code < ffi::MPV_ERROR_SUCCESS {
        return Err(MpvError::Api { call: call(), code });
    }
    Ok(())
}

fn to_cstring(string: &str) -> Result<CString, MpvError> {
    CString::new(string).map_err(|_| MpvError::InvalidString(string.to_owned()))
}

/// An owned copy of an `mpv_node`.
#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    None,
    String(String),
    Flag(bool),
    Int64(i64),
    Double(f64),
    Array(Vec<Node>),
    Map(Vec<(String, Node)>),
    /// A format this crate does not convert (e.g. byte arrays).
    Unsupported,
}

impl Node {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Node::String(string) => Some(string),
            _ => None,
        }
    }

    pub fn get(&self, key: &str) -> Option<&Node> {
        match self {
            Node::Map(entries) => entries
                .iter()
                .find_map(|(name, value)| (name == key).then_some(value)),
            _ => None,
        }
    }

    /// Deep-copies a node owned by libmpv.
    ///
    /// # Safety
    /// `node` must be a valid `mpv_node` as returned by libmpv.
    unsafe fn from_raw(node: &ffi::mpv_node) -> Node {
        unsafe {
            match node.format {
                ffi::MPV_FORMAT_NONE => Node::None,
                ffi::MPV_FORMAT_STRING => Node::String(string_from_raw(node.u.string)),
                ffi::MPV_FORMAT_FLAG => Node::Flag(node.u.flag != 0),
                ffi::MPV_FORMAT_INT64 => Node::Int64(node.u.int64),
                ffi::MPV_FORMAT_DOUBLE => Node::Double(node.u.double_),
                ffi::MPV_FORMAT_NODE_ARRAY => Node::Array(
                    list_values(node.u.list)
                        .iter()
                        .map(|value| Node::from_raw(value))
                        .collect(),
                ),
                ffi::MPV_FORMAT_NODE_MAP => {
                    let list = node.u.list;
                    let values = list_values(list);
                    let keys = if list.is_null() || (*list).keys.is_null() {
                        &[]
                    } else {
                        slice::from_raw_parts((*list).keys, values.len())
                    };
                    Node::Map(
                        keys.iter()
                            .zip(values)
                            .map(|(key, value)| (string_from_raw(*key), Node::from_raw(value)))
                            .collect(),
                    )
                },
                _ => Node::Unsupported,
            }
        }
    }
}

/// # Safety
/// `string` must be NULL or a valid NUL-terminated string.
unsafe fn string_from_raw(string: *const std::ffi::c_char) -> String {
    if string.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(string) }
        .to_string_lossy()
        .into_owned()
}

/// # Safety
/// `list` must be NULL or a valid `mpv_node_list` that outlives the returned slice.
unsafe fn list_values<'a>(list: *const ffi::mpv_node_list) -> &'a [ffi::mpv_node] {
    unsafe {
        if list.is_null() || (*list).values.is_null() || (*list).num <= 0 {
            return &[];
        }
        slice::from_raw_parts((*list).values, (*list).num as usize)
    }
}

/// Why mpv stopped playing a file.
#[derive(Clone, Debug, PartialEq)]
pub enum EndFileReason {
    /// The end of the file was reached.
    Eof,
    /// Playback failed, with libmpv's description of the error.
    Error(String),
    /// Stopped, replaced by another file, or mpv is quitting.
    Other,
}

/// An owned copy of an `mpv_event`, limited to what this crate reacts to.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// The wait timed out, or was interrupted by [`MpvHandle::wakeup`].
    None,
    /// The instance is being destroyed: no more events will come.
    Shutdown,
    FileLoaded,
    EndFile(EndFileReason),
    /// An observed property changed. `value` is [`Node::None`] when it is unavailable.
    PropertyChange {
        name: String,
        value: Node,
    },
    /// An event this crate does not handle.
    Other,
}

impl Event {
    /// Copies an event returned by `mpv_wait_event()`.
    ///
    /// # Safety
    /// `event` must be valid, and its `data` must match its `event_id` as documented in
    /// client.h. The data of property changes must have been requested as `MPV_FORMAT_NODE`.
    unsafe fn from_raw(event: &ffi::mpv_event) -> Event {
        unsafe {
            match event.event_id {
                ffi::MPV_EVENT_NONE => Event::None,
                ffi::MPV_EVENT_SHUTDOWN => Event::Shutdown,
                ffi::MPV_EVENT_FILE_LOADED => Event::FileLoaded,
                ffi::MPV_EVENT_END_FILE => {
                    let end_file = &*(event.data as *const ffi::mpv_event_end_file);
                    Event::EndFile(match end_file.reason {
                        ffi::MPV_END_FILE_REASON_EOF => EndFileReason::Eof,
                        ffi::MPV_END_FILE_REASON_ERROR => {
                            EndFileReason::Error(error_string(end_file.error))
                        },
                        _ => EndFileReason::Other,
                    })
                },
                ffi::MPV_EVENT_PROPERTY_CHANGE => {
                    let property = &*(event.data as *const ffi::mpv_event_property);
                    let value =
                        if property.format == ffi::MPV_FORMAT_NODE && !property.data.is_null() {
                            Node::from_raw(&*(property.data as *const ffi::mpv_node))
                        } else {
                            Node::None
                        };
                    Event::PropertyChange {
                        name: string_from_raw(property.name),
                        value,
                    }
                },
                _ => Event::Other,
            }
        }
    }
}

/// An initialized mpv instance, destroyed on drop.
pub struct MpvHandle {
    ctx: NonNull<ffi::mpv_handle>,
    /// Whether the [`EventQueue`] of this instance was handed out.
    events_taken: AtomicBool,
    /// The bridge behind the `aether-media` protocol. libmpv only borrows it, so it is
    /// dropped with the handle, after `mpv_terminate_destroy` closed every stream.
    _stream_bridge: OnceLock<Arc<StreamBridge>>,
}

// client.h: "The client API is generally fully thread-safe, unless otherwise noted."
// The only exception relevant here is mpv_wait_event(), which may only be called by one
// thread at a time: it is only reachable through the unique `EventQueue` of an instance.
unsafe impl Send for MpvHandle {}
unsafe impl Sync for MpvHandle {}

impl MpvHandle {
    /// Creates an mpv instance, applies `options` (which must include everything that is
    /// only read at initialization time) and initializes it.
    pub fn new(options: &[(&str, &str)]) -> Result<MpvHandle, MpvError> {
        let (major, minor) = client_api_version();
        if major != SUPPORTED_API_MAJOR {
            return Err(MpvError::UnsupportedApi { major, minor });
        }

        let ctx = NonNull::new(unsafe { ffi::mpv_create() }).ok_or(MpvError::CreateFailed)?;
        // From here on, dropping `handle` releases the instance, even if it never
        // gets initialized.
        let handle = MpvHandle {
            ctx,
            events_taken: AtomicBool::new(false),
            _stream_bridge: OnceLock::new(),
        };
        for (name, value) in options {
            handle.set_option(name, value)?;
        }
        check(unsafe { ffi::mpv_initialize(handle.ctx.as_ptr()) }, || {
            "mpv_initialize".to_owned()
        })?;
        Ok(handle)
    }

    fn set_option(&self, name: &str, value: &str) -> Result<(), MpvError> {
        let c_name = to_cstring(name)?;
        let c_value = to_cstring(value)?;
        check(
            unsafe {
                ffi::mpv_set_option_string(self.ctx.as_ptr(), c_name.as_ptr(), c_value.as_ptr())
            },
            || format!("mpv_set_option_string({name}={value})"),
        )
    }

    /// Sets a boolean (`MPV_FORMAT_FLAG`) property.
    pub fn set_flag(&self, name: &str, value: bool) -> Result<(), MpvError> {
        let c_name = to_cstring(name)?;
        let mut flag = c_int::from(value);
        check(
            unsafe {
                ffi::mpv_set_property(
                    self.ctx.as_ptr(),
                    c_name.as_ptr(),
                    ffi::MPV_FORMAT_FLAG,
                    (&raw mut flag).cast(),
                )
            },
            || format!("mpv_set_property({name}={value})"),
        )
    }

    /// Reads a boolean (`MPV_FORMAT_FLAG`) property.
    pub fn get_flag(&self, name: &str) -> Result<bool, MpvError> {
        let c_name = to_cstring(name)?;
        let mut flag: c_int = 0;
        check(
            unsafe {
                ffi::mpv_get_property(
                    self.ctx.as_ptr(),
                    c_name.as_ptr(),
                    ffi::MPV_FORMAT_FLAG,
                    (&raw mut flag).cast(),
                )
            },
            || format!("mpv_get_property({name})"),
        )?;
        Ok(flag != 0)
    }

    /// Runs a command, e.g. `["seek", "0", "absolute"]` (see `DOCS/man/input.rst`).
    pub fn command(&self, args: &[&str]) -> Result<(), MpvError> {
        let c_args = args
            .iter()
            .map(|arg| to_cstring(arg))
            .collect::<Result<Vec<_>, _>>()?;
        // mpv_command() takes a NULL-terminated list of strings.
        let mut argv: Vec<*const c_char> = c_args
            .iter()
            .map(|arg| arg.as_ptr())
            .chain(std::iter::once(ptr::null()))
            .collect();
        check(
            unsafe { ffi::mpv_command(self.ctx.as_ptr(), argv.as_mut_ptr()) },
            || format!("mpv_command({})", args.join(" ")),
        )
    }

    pub fn set_double(&self, name: &str, value: f64) -> Result<(), MpvError> {
        let c_name = to_cstring(name)?;
        let mut double = value;
        check(
            unsafe {
                ffi::mpv_set_property(
                    self.ctx.as_ptr(),
                    c_name.as_ptr(),
                    ffi::MPV_FORMAT_DOUBLE,
                    (&raw mut double).cast(),
                )
            },
            || format!("mpv_set_property({name}={value})"),
        )
    }

    /// Asks libmpv to report changes of a property as [`Event::PropertyChange`]. libmpv
    /// also reports its current value right away.
    pub fn observe(&self, name: &str) -> Result<(), MpvError> {
        let c_name = to_cstring(name)?;
        check(
            unsafe {
                ffi::mpv_observe_property(
                    self.ctx.as_ptr(),
                    0,
                    c_name.as_ptr(),
                    ffi::MPV_FORMAT_NODE,
                )
            },
            || format!("mpv_observe_property({name})"),
        )
    }

    /// Interrupts a pending [`EventQueue::wait`], which then returns [`Event::None`].
    pub fn wakeup(&self) {
        unsafe { ffi::mpv_wakeup(self.ctx.as_ptr()) };
    }

    /// The event queue of this instance. libmpv allows only one consumer, so this
    /// returns `None` once the queue was handed out.
    pub fn events(self: &Arc<Self>) -> Option<EventQueue> {
        if self.events_taken.swap(true, Ordering::AcqRel) {
            return None;
        }
        Some(EventQueue {
            handle: self.clone(),
        })
    }

    /// Reads a property as an owned [`Node`].
    pub fn get_node(&self, name: &str) -> Result<Node, MpvError> {
        let c_name = to_cstring(name)?;
        let mut raw = MaybeUninit::<ffi::mpv_node>::uninit();
        check(
            unsafe {
                ffi::mpv_get_property(
                    self.ctx.as_ptr(),
                    c_name.as_ptr(),
                    ffi::MPV_FORMAT_NODE,
                    raw.as_mut_ptr().cast(),
                )
            },
            || format!("mpv_get_property({name})"),
        )?;
        // On success, libmpv filled the node and we own its contents.
        let mut raw = unsafe { raw.assume_init() };
        let node = unsafe { Node::from_raw(&raw) };
        unsafe { ffi::mpv_free_node_contents(&mut raw) };
        Ok(node)
    }
}

/// The only consumer of the events of an mpv instance. Keeps the instance alive.
pub struct EventQueue {
    handle: Arc<MpvHandle>,
}

impl EventQueue {
    pub fn handle(&self) -> &MpvHandle {
        &self.handle
    }

    /// Waits up to `timeout` seconds (forever if negative) for the next event.
    pub fn wait(&mut self, timeout: f64) -> Event {
        // `&mut self` on the unique queue of the instance: no other thread can be in
        // mpv_wait_event(). The event stays valid until the next call, and is copied first.
        let event = unsafe { ffi::mpv_wait_event(self.handle.ctx.as_ptr(), timeout) };
        match unsafe { event.as_ref() } {
            Some(event) => unsafe { Event::from_raw(event) },
            None => Event::None,
        }
    }
}

// Custom streams: `aether-media://` URLs read from a StreamBridge.

impl MpvHandle {
    /// Makes `aether-media://` URLs read from `bridge`. Can only be done once per instance.
    ///
    /// The bridge serves a single stream: once libmpv cancels it, when it unloads the
    /// file, the bytes already read are gone and the stream cannot be opened again.
    pub fn add_stream_protocol(&self, bridge: Arc<StreamBridge>) -> Result<(), MpvError> {
        // libmpv keeps the protocol, and calls into the bridge, until the instance is
        // destroyed (stream_cb.h): the handle keeps the bridge alive until then.
        let user_data = Arc::as_ptr(&bridge).cast_mut().cast::<c_void>();
        check(
            unsafe {
                ffi::mpv_stream_cb_add_ro(
                    self.ctx.as_ptr(),
                    c"aether-media".as_ptr(),
                    user_data,
                    Some(open_stream),
                )
            },
            || "mpv_stream_cb_add_ro(aether-media)".to_owned(),
        )?;
        // libmpv refuses a protocol registered twice, so this is the first bridge.
        let _ = self._stream_bridge.set(bridge);
        Ok(())
    }
}

/// The bridge behind a stream's cookie.
///
/// # Safety
/// `cookie` must come from [`MpvHandle::add_stream_protocol`], whose handle is not
/// destroyed yet.
unsafe fn bridge<'a>(cookie: *mut c_void) -> &'a StreamBridge {
    unsafe { &*cookie.cast::<StreamBridge>() }
}

/// Runs `callback`, returning `on_panic` if it panics: a panic must not unwind into C.
fn catch_panic<T>(on_panic: T, callback: impl FnOnce() -> T) -> T {
    panic::catch_unwind(AssertUnwindSafe(callback)).unwrap_or(on_panic)
}

unsafe extern "C" fn open_stream(
    user_data: *mut c_void,
    _uri: *mut c_char,
    info: *mut ffi::mpv_stream_cb_info,
) -> c_int {
    catch_panic(ffi::MPV_ERROR_LOADING_FAILED, || {
        if unsafe { bridge(user_data) }.is_cancelled() {
            return ffi::MPV_ERROR_LOADING_FAILED;
        }
        // Every stream of the instance reads from the same bridge, which outlives them.
        let info = unsafe { &mut *info };
        info.cookie = user_data;
        info.read_fn = Some(read_stream);
        info.seek_fn = Some(seek_stream);
        info.size_fn = Some(stream_size);
        info.close_fn = Some(close_stream);
        info.cancel_fn = Some(cancel_stream);
        0
    })
}

unsafe extern "C" fn read_stream(cookie: *mut c_void, buf: *mut c_char, nbytes: u64) -> i64 {
    catch_panic(-1, || {
        let len = usize::try_from(nbytes).unwrap_or(usize::MAX);
        let buf = unsafe { slice::from_raw_parts_mut(buf.cast::<u8>(), len) };
        match unsafe { bridge(cookie) }.read(buf) {
            ReadResult::Data(count) => i64::try_from(count).unwrap_or(-1),
            ReadResult::EndOfStream => 0,
            ReadResult::Cancelled => -1,
        }
    })
}

unsafe extern "C" fn seek_stream(cookie: *mut c_void, offset: i64) -> i64 {
    let unsupported = i64::from(ffi::MPV_ERROR_UNSUPPORTED);
    catch_panic(i64::from(ffi::MPV_ERROR_GENERIC), || {
        u64::try_from(offset)
            .ok()
            .and_then(|offset| unsafe { bridge(cookie) }.seek(offset))
            .and_then(|position| i64::try_from(position).ok())
            .unwrap_or(unsupported)
    })
}

unsafe extern "C" fn stream_size(cookie: *mut c_void) -> i64 {
    let unsupported = i64::from(ffi::MPV_ERROR_UNSUPPORTED);
    catch_panic(unsupported, || {
        unsafe { bridge(cookie) }
            .size()
            .and_then(|size| i64::try_from(size).ok())
            .unwrap_or(unsupported)
    })
}

/// The bridge belongs to the handle: closing a stream releases nothing.
unsafe extern "C" fn close_stream(_cookie: *mut c_void) {}

unsafe extern "C" fn cancel_stream(cookie: *mut c_void) {
    catch_panic((), || unsafe { bridge(cookie) }.cancel());
}

impl Drop for MpvHandle {
    fn drop(&mut self) {
        // Blocks until the core has shut down (audio output closed, threads joined).
        unsafe { ffi::mpv_terminate_destroy(self.ctx.as_ptr()) };
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::stream::{BridgeObserver, SeekReply};

    /// An observer for streams that never need Servo to seek.
    struct NoSeek;

    impl BridgeObserver for NoSeek {
        fn need_data(&self) {}
        fn enough_data(&self) {}
        fn seek_data(&self, _: u64, _: &StreamBridge) -> SeekReply {
            SeekReply {
                data_will_come: false,
                ack: Box::new(|| {}),
            }
        }
    }

    /// A bridge holding a whole test file, as Servo would push it.
    fn bridge_with_test_file(seekable: bool) -> Arc<StreamBridge> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../tests/wpt/tests/media/sine440.mp3");
        let data = std::fs::read(path).unwrap();
        let bridge = Arc::new(StreamBridge::new(Box::new(NoSeek)));
        bridge.set_seekable(seekable);
        bridge.set_size(data.len() as u64);
        for chunk in data.chunks(64 * 1024) {
            bridge.push(chunk.to_vec());
        }
        bridge.end_of_stream();
        bridge
    }

    /// Waits until `condition` holds, failing the test after a few seconds.
    fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !condition() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn time_pos(handle: &MpvHandle) -> f64 {
        match handle.get_node("time-pos") {
            Ok(Node::Double(seconds)) => seconds,
            _ => 0.0,
        }
    }

    /// Plays `aether-media://0` from `bridge`, checking that it gets loaded and plays.
    fn play_from(bridge: Arc<StreamBridge>) -> MpvHandle {
        let handle = MpvHandle::new(&[("ao", "null"), ("vid", "no")]).unwrap();
        handle.add_stream_protocol(bridge).unwrap();
        handle.command(&["loadfile", "aether-media://0"]).unwrap();
        wait_until("the stream to load", || {
            matches!(handle.get_node("duration"), Ok(Node::Double(_)))
        });
        wait_until("playback to advance", || time_pos(&handle) > 0.1);
        handle
    }

    #[test]
    fn linked_libmpv_speaks_the_supported_api() {
        assert_eq!(client_api_version().0, SUPPORTED_API_MAJOR);
    }

    #[test]
    fn handles_are_created_and_destroyed() {
        drop(MpvHandle::new(&[("ao", "null"), ("vid", "no")]).unwrap());
    }

    #[test]
    fn invalid_options_are_reported() {
        let error = MpvHandle::new(&[("not-an-option", "1")]).err().unwrap();
        assert!(
            error
                .to_string()
                .starts_with("mpv_set_option_string(not-an-option=1) failed: ")
        );
        assert_eq!(
            MpvHandle::new(&[("vid", "n\0o")]).err(),
            Some(MpvError::InvalidString("n\0o".to_owned()))
        );
    }

    #[test]
    fn flags_are_written_and_read() {
        let handle = MpvHandle::new(&[("ao", "null"), ("vid", "no")]).unwrap();
        handle.set_flag("pause", true).unwrap();
        assert_eq!(handle.get_node("pause"), Ok(Node::Flag(true)));
        assert_eq!(handle.get_flag("pause"), Ok(true));
        handle.set_flag("pause", false).unwrap();
        assert_eq!(handle.get_node("pause"), Ok(Node::Flag(false)));
        assert_eq!(handle.get_flag("pause"), Ok(false));
        assert!(matches!(
            handle.get_flag("not-a-property"),
            Err(MpvError::Api { .. })
        ));
        assert!(matches!(
            handle.set_flag("not-a-property", true),
            Err(MpvError::Api { .. })
        ));
    }

    #[test]
    fn commands_are_run() {
        let handle = MpvHandle::new(&[("ao", "null"), ("vid", "no")]).unwrap();
        handle.command(&["set", "pause", "yes"]).unwrap();
        assert_eq!(handle.get_flag("pause"), Ok(true));
        let error = handle.command(&["not-a-command"]).unwrap_err();
        assert!(
            error
                .to_string()
                .starts_with("mpv_command(not-a-command) failed: ")
        );
    }

    #[test]
    fn doubles_are_written() {
        let handle = MpvHandle::new(&[("ao", "null"), ("vid", "no")]).unwrap();
        handle.set_double("volume", 42.5).unwrap();
        assert_eq!(handle.get_node("volume"), Ok(Node::Double(42.5)));
        assert!(matches!(
            handle.set_double("not-a-property", 1.0),
            Err(MpvError::Api { .. })
        ));
    }

    #[test]
    fn properties_are_read_as_owned_nodes() {
        let handle = MpvHandle::new(&[("ao", "null"), ("vid", "no")]).unwrap();
        let version = handle.get_node("mpv-version").unwrap();
        assert!(version.as_str().is_some_and(|version| !version.is_empty()));
        assert!(matches!(
            handle.get_node("not-a-property"),
            Err(MpvError::Api { .. })
        ));
    }

    #[test]
    fn a_seekable_stream_is_played_from_the_bridge() {
        let handle = play_from(bridge_with_test_file(true));
        assert_eq!(handle.get_node("seekable"), Ok(Node::Flag(true)));
    }

    #[test]
    fn a_stream_that_is_not_seekable_is_played_linearly() {
        let handle = play_from(bridge_with_test_file(false));
        assert_eq!(handle.get_node("seekable"), Ok(Node::Flag(false)));
    }

    #[test]
    fn the_stream_size_comes_from_the_bridge() {
        let bridge = bridge_with_test_file(true);
        let size = bridge.size().unwrap();
        let handle = play_from(bridge);
        assert_eq!(handle.get_node("stream-end"), Ok(Node::Int64(size as i64)));
    }

    #[test]
    fn the_stream_protocol_is_registered_once() {
        let handle = MpvHandle::new(&[("ao", "null"), ("vid", "no")]).unwrap();
        let bridge = || Arc::new(StreamBridge::new(Box::new(NoSeek)));
        handle.add_stream_protocol(bridge()).unwrap();
        assert!(matches!(
            handle.add_stream_protocol(bridge()),
            Err(MpvError::Api { .. })
        ));
    }

    #[test]
    fn destroying_the_handle_cancels_a_waiting_read() {
        // Nothing is pushed: libmpv's demuxer waits in `read` while it opens the stream.
        let bridge = Arc::new(StreamBridge::new(Box::new(NoSeek)));
        bridge.set_seekable(true);
        let handle = MpvHandle::new(&[("ao", "null"), ("vid", "no")]).unwrap();
        handle.add_stream_protocol(bridge.clone()).unwrap();
        handle.command(&["loadfile", "aether-media://0"]).unwrap();
        thread::sleep(Duration::from_millis(200));

        let (done, destroyed) = mpsc::channel();
        thread::spawn(move || {
            drop(handle);
            let _ = done.send(());
        });
        assert!(
            destroyed.recv_timeout(Duration::from_secs(5)).is_ok(),
            "mpv_terminate_destroy waited forever for the read"
        );
        assert!(bridge.is_cancelled());
    }

    #[test]
    fn a_cancelled_bridge_cannot_be_opened_again() {
        let bridge = bridge_with_test_file(true);
        let handle = Arc::new(play_from(bridge.clone()));
        let mut events = handle.events().unwrap();
        // What libmpv does when it unloads the file.
        bridge.cancel();
        handle.command(&["loadfile", "aether-media://0"]).unwrap();

        // The first file ends as it is replaced, then the second one fails to open,
        // instead of failing to read as a file of an "unrecognized format".
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut ends = vec![];
        while ends.len() < 2 {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for the files to end"
            );
            if let Event::EndFile(reason) = events.wait(1.0) {
                ends.push(reason);
            }
        }
        assert_eq!(
            ends[1],
            EndFileReason::Error(error_string(ffi::MPV_ERROR_LOADING_FAILED))
        );
    }
}
