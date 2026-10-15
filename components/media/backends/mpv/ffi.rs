/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Hand-written bindings to the subset of libmpv's `client.h` used by this crate.
//!
//! Layouts and values are checked against `mpv/client.h` for client API 2.5. The mpv
//! public headers are ISC licensed (Copyright (C) the mpv developers). Linking is set up
//! by `build.rs`.
//!
//! Nothing outside of `handle.rs` should use this module directly.

use std::ffi::{c_char, c_int, c_ulong};

/// Opaque `mpv_handle`.
#[repr(C)]
pub struct mpv_handle {
    _private: [u8; 0],
}

/// `mpv_error`. Negative values are errors.
pub const MPV_ERROR_SUCCESS: c_int = 0;

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
}
