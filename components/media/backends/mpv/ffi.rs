/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Hand-written bindings to the subset of libmpv's `client.h` and `stream_cb.h` used by
//! this crate.
//!
//! Layouts and values are checked against `mpv/client.h` and `mpv/stream_cb.h` for client
//! API 2.5. The mpv
//! public headers are ISC licensed (Copyright (C) the mpv developers). Linking is set up
//! by `build.rs`.
//!
//! Nothing outside of `handle.rs` should use this module directly.

use std::ffi::{c_char, c_int, c_ulong, c_void};

/// Opaque `mpv_handle`.
#[repr(C)]
pub struct mpv_handle {
    _private: [u8; 0],
}

/// `mpv_format`.
pub type mpv_format = c_int;
pub const MPV_FORMAT_NONE: mpv_format = 0;
pub const MPV_FORMAT_STRING: mpv_format = 1;
pub const MPV_FORMAT_FLAG: mpv_format = 3;
pub const MPV_FORMAT_INT64: mpv_format = 4;
pub const MPV_FORMAT_DOUBLE: mpv_format = 5;
pub const MPV_FORMAT_NODE: mpv_format = 6;
pub const MPV_FORMAT_NODE_ARRAY: mpv_format = 7;
pub const MPV_FORMAT_NODE_MAP: mpv_format = 8;

/// `mpv_error`. Negative values are errors.
pub const MPV_ERROR_SUCCESS: c_int = 0;
pub const MPV_ERROR_LOADING_FAILED: c_int = -13;
pub const MPV_ERROR_UNSUPPORTED: c_int = -18;
pub const MPV_ERROR_GENERIC: c_int = -20;

/// `mpv_node`: a tagged union, `format` says which member of `u` is valid.
#[repr(C)]
pub struct mpv_node {
    pub u: mpv_node_u,
    pub format: mpv_format,
}

#[repr(C)]
pub union mpv_node_u {
    pub string: *mut c_char,
    pub flag: c_int,
    pub int64: i64,
    pub double_: f64,
    pub list: *mut mpv_node_list,
    pub ba: *mut c_void,
}

/// `mpv_node_list`: `keys` is NULL for `MPV_FORMAT_NODE_ARRAY`.
#[repr(C)]
pub struct mpv_node_list {
    pub num: c_int,
    pub values: *mut mpv_node,
    pub keys: *mut *mut c_char,
}

/// `mpv_event_id`.
pub type mpv_event_id = c_int;
pub const MPV_EVENT_NONE: mpv_event_id = 0;
pub const MPV_EVENT_SHUTDOWN: mpv_event_id = 1;
pub const MPV_EVENT_END_FILE: mpv_event_id = 7;
pub const MPV_EVENT_FILE_LOADED: mpv_event_id = 8;
pub const MPV_EVENT_PLAYBACK_RESTART: mpv_event_id = 21;
pub const MPV_EVENT_PROPERTY_CHANGE: mpv_event_id = 22;

/// `mpv_end_file_reason`.
pub type mpv_end_file_reason = c_int;
pub const MPV_END_FILE_REASON_EOF: mpv_end_file_reason = 0;
pub const MPV_END_FILE_REASON_ERROR: mpv_end_file_reason = 4;

/// `mpv_event`: `data` points to a struct that depends on `event_id`.
#[repr(C)]
pub struct mpv_event {
    pub event_id: mpv_event_id,
    pub error: c_int,
    pub reply_userdata: u64,
    pub data: *mut c_void,
}

/// `mpv_event_property`, the `data` of `MPV_EVENT_PROPERTY_CHANGE`. `format` is
/// `MPV_FORMAT_NONE` (and `data` NULL) when the property is unavailable.
#[repr(C)]
pub struct mpv_event_property {
    pub name: *const c_char,
    pub format: mpv_format,
    pub data: *mut c_void,
}

/// `mpv_event_end_file`, the `data` of `MPV_EVENT_END_FILE`.
#[repr(C)]
pub struct mpv_event_end_file {
    pub reason: mpv_end_file_reason,
    pub error: c_int,
    pub playlist_entry_id: i64,
    pub playlist_insert_id: i64,
    pub playlist_insert_num_entries: c_int,
}

#[cfg(has_libmpv)]
unsafe extern "C" {
    pub fn mpv_client_api_version() -> c_ulong;
    pub fn mpv_error_string(error: c_int) -> *const c_char;

    pub fn mpv_create() -> *mut mpv_handle;
    pub fn mpv_initialize(ctx: *mut mpv_handle) -> c_int;
    pub fn mpv_terminate_destroy(ctx: *mut mpv_handle);

    pub fn mpv_set_option_string(
        ctx: *mut mpv_handle,
        name: *const c_char,
        data: *const c_char,
    ) -> c_int;
    pub fn mpv_set_property(
        ctx: *mut mpv_handle,
        name: *const c_char,
        format: mpv_format,
        data: *mut c_void,
    ) -> c_int;
    pub fn mpv_command(ctx: *mut mpv_handle, args: *mut *const c_char) -> c_int;
    pub fn mpv_get_property(
        ctx: *mut mpv_handle,
        name: *const c_char,
        format: mpv_format,
        data: *mut c_void,
    ) -> c_int;

    pub fn mpv_free_node_contents(node: *mut mpv_node);

    pub fn mpv_observe_property(
        ctx: *mut mpv_handle,
        reply_userdata: u64,
        name: *const c_char,
        format: mpv_format,
    ) -> c_int;
    pub fn mpv_wait_event(ctx: *mut mpv_handle, timeout: f64) -> *mut mpv_event;
    pub fn mpv_wakeup(ctx: *mut mpv_handle);

    pub fn mpv_stream_cb_add_ro(
        ctx: *mut mpv_handle,
        protocol: *const c_char,
        user_data: *mut c_void,
        open_fn: mpv_stream_cb_open_ro_fn,
    ) -> c_int;
}

/// Stand-ins so the crate still links when pkg-config cannot find libmpv. Tests that
/// actually play media are compiled out in that case (`has_libmpv`).
#[cfg(not(has_libmpv))]
mod missing_libmpv {
    use super::*;
    use std::ptr;

    pub unsafe fn mpv_client_api_version() -> c_ulong {
        0
    }

    pub unsafe fn mpv_error_string(_error: c_int) -> *const c_char {
        c"libmpv is not available".as_ptr()
    }

    pub unsafe fn mpv_create() -> *mut mpv_handle {
        ptr::null_mut()
    }

    pub unsafe fn mpv_initialize(_ctx: *mut mpv_handle) -> c_int {
        MPV_ERROR_GENERIC
    }

    pub unsafe fn mpv_terminate_destroy(_ctx: *mut mpv_handle) {}

    pub unsafe fn mpv_set_option_string(
        _ctx: *mut mpv_handle,
        _name: *const c_char,
        _data: *const c_char,
    ) -> c_int {
        MPV_ERROR_GENERIC
    }

    pub unsafe fn mpv_set_property(
        _ctx: *mut mpv_handle,
        _name: *const c_char,
        _format: mpv_format,
        _data: *mut c_void,
    ) -> c_int {
        MPV_ERROR_GENERIC
    }

    pub unsafe fn mpv_command(_ctx: *mut mpv_handle, _args: *mut *const c_char) -> c_int {
        MPV_ERROR_GENERIC
    }

    pub unsafe fn mpv_get_property(
        _ctx: *mut mpv_handle,
        _name: *const c_char,
        _format: mpv_format,
        _data: *mut c_void,
    ) -> c_int {
        MPV_ERROR_GENERIC
    }

    pub unsafe fn mpv_free_node_contents(_node: *mut mpv_node) {}

    pub unsafe fn mpv_observe_property(
        _ctx: *mut mpv_handle,
        _reply_userdata: u64,
        _name: *const c_char,
        _format: mpv_format,
    ) -> c_int {
        MPV_ERROR_GENERIC
    }

    pub unsafe fn mpv_wait_event(_ctx: *mut mpv_handle, _timeout: f64) -> *mut mpv_event {
        static mut NONE_EVENT: mpv_event = mpv_event {
            event_id: MPV_EVENT_NONE,
            error: 0,
            reply_userdata: 0,
            data: ptr::null_mut(),
        };
        &raw mut NONE_EVENT
    }

    pub unsafe fn mpv_wakeup(_ctx: *mut mpv_handle) {}

    pub unsafe fn mpv_stream_cb_add_ro(
        _ctx: *mut mpv_handle,
        _protocol: *const c_char,
        _user_data: *mut c_void,
        _open_fn: mpv_stream_cb_open_ro_fn,
    ) -> c_int {
        MPV_ERROR_GENERIC
    }
}

#[cfg(not(has_libmpv))]
pub use missing_libmpv::*;

// stream_cb.h. Every callback may be NULL except `read_fn` and `close_fn`.

pub type mpv_stream_cb_read_fn =
    Option<unsafe extern "C" fn(cookie: *mut c_void, buf: *mut c_char, nbytes: u64) -> i64>;
pub type mpv_stream_cb_seek_fn =
    Option<unsafe extern "C" fn(cookie: *mut c_void, offset: i64) -> i64>;
pub type mpv_stream_cb_size_fn = Option<unsafe extern "C" fn(cookie: *mut c_void) -> i64>;
pub type mpv_stream_cb_close_fn = Option<unsafe extern "C" fn(cookie: *mut c_void)>;
pub type mpv_stream_cb_cancel_fn = Option<unsafe extern "C" fn(cookie: *mut c_void)>;

/// `mpv_stream_cb_info`, filled by the open callback.
#[repr(C)]
pub struct mpv_stream_cb_info {
    pub cookie: *mut c_void,
    pub read_fn: mpv_stream_cb_read_fn,
    pub seek_fn: mpv_stream_cb_seek_fn,
    pub size_fn: mpv_stream_cb_size_fn,
    pub close_fn: mpv_stream_cb_close_fn,
    pub cancel_fn: mpv_stream_cb_cancel_fn,
}

pub type mpv_stream_cb_open_ro_fn = Option<
    unsafe extern "C" fn(
        user_data: *mut c_void,
        uri: *mut c_char,
        info: *mut mpv_stream_cb_info,
    ) -> c_int,
>;
