//! Raw FFI bindings to libmpv (client.h, stream_cb.h, render.h).
//! Hand-written subset for the spike — replaces what bindgen would generate.

#![allow(dead_code)]

use std::os::raw::{c_char, c_double, c_int, c_void};

// --- Opaque handles ---

pub enum MpvHandle {}
pub enum MpvRenderContext {}

// --- client.h — mpv_format ---

pub const MPV_FORMAT_NONE: c_int = 0;
pub const MPV_FORMAT_STRING: c_int = 1;
pub const MPV_FORMAT_OSD_STRING: c_int = 2;
pub const MPV_FORMAT_FLAG: c_int = 3;
pub const MPV_FORMAT_INT64: c_int = 4;
pub const MPV_FORMAT_DOUBLE: c_int = 5;
pub const MPV_FORMAT_NODE: c_int = 6;

// --- client.h — event IDs ---

pub const MPV_EVENT_NONE: c_int = 0;
pub const MPV_EVENT_SHUTDOWN: c_int = 1;
pub const MPV_EVENT_LOG_MESSAGE: c_int = 2;
pub const MPV_EVENT_START_FILE: c_int = 6;
pub const MPV_EVENT_END_FILE: c_int = 7;
pub const MPV_EVENT_FILE_LOADED: c_int = 8;
pub const MPV_EVENT_IDLE: c_int = 11;
pub const MPV_EVENT_VIDEO_RECONFIG: c_int = 17;
pub const MPV_EVENT_AUDIO_RECONFIG: c_int = 18;
pub const MPV_EVENT_SEEK: c_int = 20;
pub const MPV_EVENT_PLAYBACK_RESTART: c_int = 21;
pub const MPV_EVENT_PROPERTY_CHANGE: c_int = 22;
pub const MPV_EVENT_QUEUE_OVERFLOW: c_int = 24;

// --- client.h — end-file reasons ---

pub const MPV_END_FILE_REASON_EOF: c_int = 0;
pub const MPV_END_FILE_REASON_STOP: c_int = 2;
pub const MPV_END_FILE_REASON_QUIT: c_int = 3;
pub const MPV_END_FILE_REASON_ERROR: c_int = 4;
pub const MPV_END_FILE_REASON_REDIRECT: c_int = 5;

// --- client.h — event structs ---

// Valid only until the next mpv_wait_event() call on the same handle.
#[repr(C)]
pub struct MpvEvent {
    pub event_id: c_int,
    pub error: c_int,
    pub reply_userdata: u64,
    pub data: *mut c_void,
}

#[repr(C)]
pub struct MpvEventProperty {
    pub name: *const c_char,
    pub format: c_int,
    pub data: *mut c_void,
}

// Shortened layout — only reason + error, compatible with mpv >= 0.35.
#[repr(C)]
pub struct MpvEventEndFile {
    pub reason: c_int,
    pub error: c_int,
}

#[repr(C)]
pub struct MpvEventLogMessage {
    pub prefix: *const c_char,
    pub level: *const c_char,
    pub text: *const c_char,
    pub log_level: c_int,
}

// --- stream_cb.h — custom protocol callbacks ---

pub type MpvStreamCbReadFn =
    unsafe extern "C" fn(cookie: *mut c_void, buf: *mut c_char, nbytes: u64) -> i64;

pub type MpvStreamCbSeekFn = unsafe extern "C" fn(cookie: *mut c_void, offset: i64) -> i64;

pub type MpvStreamCbSizeFn = unsafe extern "C" fn(cookie: *mut c_void) -> i64;

pub type MpvStreamCbCloseFn = unsafe extern "C" fn(cookie: *mut c_void);

pub type MpvStreamCbCancelFn = unsafe extern "C" fn(cookie: *mut c_void) -> i64;

pub type MpvStreamCbOpenFn = unsafe extern "C" fn(
    user_data: *mut c_void,
    uri: *const c_char,
    info: *mut MpvStreamCbInfo,
) -> c_int;

#[repr(C)]
pub struct MpvStreamCbInfo {
    pub cookie: *mut c_void,
    pub read_fn: Option<MpvStreamCbReadFn>,
    pub seek_fn: Option<MpvStreamCbSeekFn>,
    pub size_fn: Option<MpvStreamCbSizeFn>,
    pub close_fn: Option<MpvStreamCbCloseFn>,
    pub cancel_fn: Option<MpvStreamCbCancelFn>,
}

// --- render.h — software frame extraction ---

pub const MPV_RENDER_PARAM_INVALID: c_int = 0;
pub const MPV_RENDER_PARAM_API_TYPE: c_int = 1;
pub const MPV_RENDER_PARAM_SW_SIZE: c_int = 17;
pub const MPV_RENDER_PARAM_SW_FORMAT: c_int = 18;
pub const MPV_RENDER_PARAM_SW_STRIDE: c_int = 19;
pub const MPV_RENDER_PARAM_SW_POINTER: c_int = 20;

pub const MPV_RENDER_UPDATE_FRAME: u64 = 1;

#[repr(C)]
pub struct MpvRenderParam {
    pub type_: c_int,
    pub data: *mut c_void,
}

// Called from an mpv internal thread — must not call any mpv API.
pub type MpvRenderUpdateFn = unsafe extern "C" fn(cb_ctx: *mut c_void);

// --- Functions ---

#[link(name = "mpv")]
unsafe extern "C" {

    // Lifecycle
    pub fn mpv_client_api_version() -> u64;
    pub fn mpv_create() -> *mut MpvHandle;
    pub fn mpv_initialize(ctx: *mut MpvHandle) -> c_int;
    pub fn mpv_terminate_destroy(ctx: *mut MpvHandle);

    // Configuration (before mpv_initialize)
    pub fn mpv_set_option_string(
        ctx: *mut MpvHandle,
        name: *const c_char,
        data: *const c_char,
    ) -> c_int;

    // Properties
    pub fn mpv_set_property(
        ctx: *mut MpvHandle,
        name: *const c_char,
        format: c_int,
        data: *const c_void,
    ) -> c_int;

    pub fn mpv_get_property(
        ctx: *mut MpvHandle,
        name: *const c_char,
        format: c_int,
        data: *mut c_void,
    ) -> c_int;

    pub fn mpv_set_property_string(
        ctx: *mut MpvHandle,
        name: *const c_char,
        data: *const c_char,
    ) -> c_int;

    pub fn mpv_get_property_string(ctx: *mut MpvHandle, name: *const c_char) -> *mut c_char;

    pub fn mpv_free(data: *mut c_void);

    // Commands
    pub fn mpv_command(ctx: *mut MpvHandle, args: *const *const c_char) -> c_int;

    // Events
    pub fn mpv_observe_property(
        mpv: *mut MpvHandle,
        reply_userdata: u64,
        name: *const c_char,
        format: c_int,
    ) -> c_int;

    pub fn mpv_wait_event(ctx: *mut MpvHandle, timeout: c_double) -> *const MpvEvent;

    pub fn mpv_error_string(error: c_int) -> *const c_char;
    pub fn mpv_event_name(event: c_int) -> *const c_char;
    pub fn mpv_request_log_messages(ctx: *mut MpvHandle, min_level: *const c_char) -> c_int;

    // stream_cb.h
    // Must be called before mpv_initialize().
    pub fn mpv_stream_cb_add_ro(
        ctx: *mut MpvHandle,
        protocol: *const c_char,
        userdata: *mut c_void,
        open_fn: MpvStreamCbOpenFn,
    ) -> c_int;

    // render.h
    pub fn mpv_render_context_create(
        res: *mut *mut MpvRenderContext,
        mpv: *mut MpvHandle,
        params: *mut MpvRenderParam,
    ) -> c_int;

    // Callback must not call any mpv function — just signal your thread.
    pub fn mpv_render_context_set_update_callback(
        ctx: *mut MpvRenderContext,
        callback: MpvRenderUpdateFn,
        callback_ctx: *mut c_void,
    );

    pub fn mpv_render_context_update(ctx: *mut MpvRenderContext) -> u64;

    pub fn mpv_render_context_render(
        ctx: *mut MpvRenderContext,
        params: *mut MpvRenderParam,
    ) -> c_int;

    // Must be called before mpv_terminate_destroy.
    pub fn mpv_render_context_free(ctx: *mut MpvRenderContext);
}
