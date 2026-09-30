//! Raw FFI bindings to libmpv (client.h, stream_cb.h, render.h).
//!
//! This file declares only the subset of libmpv's C API needed by the Aether
//! MPV backend spike. It is a manual equivalent of what `libmpv-sys` generates
//! with bindgen — we write it by hand to understand every function we call.
//!
//! Target: libmpv client API >= 2.0 (mpv >= 0.35).
//!
//! Reference headers:
//!   - client.h    — lifecycle, properties, commands, events
//!   - stream_cb.h — custom protocol registration (push-to-pull bridge)
//!   - render.h    — software frame extraction
//!
//! Safety: every function in the `extern "C"` block is unsafe to call.
//! The caller must ensure the `MpvHandle` pointer is valid and that
//! string arguments are valid null-terminated C strings.

#![allow(dead_code)]

use std::os::raw::{c_char, c_double, c_int, c_void};

// ═══════════════════════════════════════════════════════════════════════════
// Opaque handles
//
// Empty enums that can never be instantiated in Rust. We only ever hold
// `*mut MpvHandle` pointers that come from C — the empty enum gives us
// type safety (you can't mix up a MpvHandle* with a MpvRenderContext*).
// ═══════════════════════════════════════════════════════════════════════════

pub enum MpvHandle {}
pub enum MpvRenderContext {}

// ═══════════════════════════════════════════════════════════════════════════
// client.h — property formats
//
// When reading or writing an mpv property, we must tell mpv what type we
// expect. These constants are the `mpv_format` enum values.
//
// Maps to: Player::set_volume (DOUBLE), Player::set_mute (FLAG), etc.
// ═══════════════════════════════════════════════════════════════════════════

pub const MPV_FORMAT_NONE: c_int = 0;
pub const MPV_FORMAT_STRING: c_int = 1;
pub const MPV_FORMAT_OSD_STRING: c_int = 2;
/// Boolean: 0 = false, 1 = true. Used for "pause", "mute", etc.
pub const MPV_FORMAT_FLAG: c_int = 3;
pub const MPV_FORMAT_INT64: c_int = 4;
/// f64. Used for "volume", "speed", "time-pos", etc.
pub const MPV_FORMAT_DOUBLE: c_int = 5;
pub const MPV_FORMAT_NODE: c_int = 6;

// ═══════════════════════════════════════════════════════════════════════════
// client.h — event IDs
//
// mpv is asynchronous: commands don't block, they produce events.
// We poll events with mpv_wait_event() and dispatch on the event_id.
//
// Maps to: PlayerEvent::StateChanged, EndOfStream, PositionChanged, etc.
// ═══════════════════════════════════════════════════════════════════════════

/// Returned by mpv_wait_event() when the timeout expires with no event.
pub const MPV_EVENT_NONE: c_int = 0;
/// mpv is shutting down. Stop the event loop.
pub const MPV_EVENT_SHUTDOWN: c_int = 1;
/// A log message (if log level was requested).
pub const MPV_EVENT_LOG_MESSAGE: c_int = 2;
/// A file has started loading.
pub const MPV_EVENT_START_FILE: c_int = 6;
/// Playback ended. data points to MpvEventEndFile (reason + error).
pub const MPV_EVENT_END_FILE: c_int = 7;
/// File was loaded and headers/metadata are available. Playback begins.
pub const MPV_EVENT_FILE_LOADED: c_int = 8;
/// The player entered idle mode (nothing to play).
pub const MPV_EVENT_IDLE: c_int = 11;
/// Video output was reconfigured (resolution or format changed).
pub const MPV_EVENT_VIDEO_RECONFIG: c_int = 17;
/// Audio output was reconfigured.
pub const MPV_EVENT_AUDIO_RECONFIG: c_int = 18;
/// Seek started.
pub const MPV_EVENT_SEEK: c_int = 20;
/// Playback restarted after a seek or format change.
pub const MPV_EVENT_PLAYBACK_RESTART: c_int = 21;
/// An observed property changed. data points to MpvEventProperty.
pub const MPV_EVENT_PROPERTY_CHANGE: c_int = 22;
/// The event queue overflowed. Events were lost.
pub const MPV_EVENT_QUEUE_OVERFLOW: c_int = 24;

// ═══════════════════════════════════════════════════════════════════════════
// client.h — end-file reasons (payload of MPV_EVENT_END_FILE)
// ═══════════════════════════════════════════════════════════════════════════

/// Playback reached end of file normally.
pub const MPV_END_FILE_REASON_EOF: c_int = 0;
/// Playback was stopped by a "stop" command.
pub const MPV_END_FILE_REASON_STOP: c_int = 2;
/// Player is quitting entirely.
pub const MPV_END_FILE_REASON_QUIT: c_int = 3;
/// An error occurred during playback.
pub const MPV_END_FILE_REASON_ERROR: c_int = 4;
/// The file was a redirect to another URL.
pub const MPV_END_FILE_REASON_REDIRECT: c_int = 5;

// ═══════════════════════════════════════════════════════════════════════════
// client.h — event structs
//
// When mpv_wait_event() returns an event, its `data` field points to one
// of these structs depending on the event_id:
//   - PROPERTY_CHANGE → MpvEventProperty
//   - END_FILE        → MpvEventEndFile
//   - LOG_MESSAGE     → MpvEventLogMessage
//   - all others      → data is NULL
// ═══════════════════════════════════════════════════════════════════════════

/// Top-level event returned by mpv_wait_event(). Owned by mpv — valid only
/// until the next mpv_wait_event() call on the same handle.
#[repr(C)]
pub struct MpvEvent {
    pub event_id: c_int,
    pub error: c_int,
    pub reply_userdata: u64,
    /// Points to an event-specific struct, or NULL. See above.
    pub data: *mut c_void,
}

/// Payload for MPV_EVENT_PROPERTY_CHANGE.
/// `data` points to a value in the format described by `format`,
/// or is NULL if the property is unavailable.
#[repr(C)]
pub struct MpvEventProperty {
    pub name: *const c_char,
    pub format: c_int,
    pub data: *mut c_void,
}

/// Payload for MPV_EVENT_END_FILE.
/// Note: mpv >= 0.38 adds extra fields after `error`, but we only read
/// `reason` and `error`, so this shortened struct is layout-compatible.
#[repr(C)]
pub struct MpvEventEndFile {
    pub reason: c_int,
    pub error: c_int,
}

/// Payload for MPV_EVENT_LOG_MESSAGE.
#[repr(C)]
pub struct MpvEventLogMessage {
    pub prefix: *const c_char,
    pub level: *const c_char,
    pub text: *const c_char,
    pub log_level: c_int,
}

// ═══════════════════════════════════════════════════════════════════════════
// stream_cb.h — custom protocol callbacks
//
// This is THE critical API for Aether. It lets us register a custom URL
// protocol ("aether-media://") so mpv reads media data from our code
// instead of from a file or network.
//
// Flow:
//   1. We call mpv_stream_cb_add_ro(mpv, "aether-media", userdata, open_fn)
//   2. We tell mpv to load "aether-media://stream-42"
//   3. mpv calls our open_fn → we fill MpvStreamCbInfo with our callbacks
//   4. mpv calls read_fn(cookie, buf, n) whenever it needs bytes
//   5. mpv calls close_fn(cookie) when done
//
// The `cookie` pointer (set in open_fn) is our StreamBridge — the shared
// buffer between Servo's push side and mpv's pull side.
//
// SAFETY: mpv calls these callbacks from its demuxer thread, not the main
// thread. The data behind `cookie` must be thread-safe.
// ═══════════════════════════════════════════════════════════════════════════

/// Read up to `nbytes` into `buf`. Return bytes read, 0 on EOF, -1 on error.
pub type MpvStreamCbReadFn =
    unsafe extern "C" fn(cookie: *mut c_void, buf: *mut c_char, nbytes: u64) -> i64;

/// Seek to `offset` (bytes from start). Return new position, or -1 on error.
pub type MpvStreamCbSeekFn =
    unsafe extern "C" fn(cookie: *mut c_void, offset: i64) -> i64;

/// Return total stream size in bytes, or -1 if unknown.
pub type MpvStreamCbSizeFn =
    unsafe extern "C" fn(cookie: *mut c_void) -> i64;

/// Called when mpv closes the stream. Free resources behind `cookie` here.
pub type MpvStreamCbCloseFn =
    unsafe extern "C" fn(cookie: *mut c_void);

/// Cancel a blocking read. Added in mpv API 2.3.
pub type MpvStreamCbCancelFn =
    unsafe extern "C" fn(cookie: *mut c_void) -> i64;

/// Called by mpv when it opens a URL matching our protocol.
/// We fill `info` with our callback pointers and set `info.cookie`.
/// Return 0 on success, -1 on error.
pub type MpvStreamCbOpenFn = unsafe extern "C" fn(
    user_data: *mut c_void,
    uri: *const c_char,
    info: *mut MpvStreamCbInfo,
) -> c_int;

/// Struct passed to open_fn. mpv initializes all fields to NULL/None.
/// We fill in the callbacks we support.
#[repr(C)]
pub struct MpvStreamCbInfo {
    /// Opaque pointer passed as first argument to every callback below.
    pub cookie: *mut c_void,
    /// Required. Must always be set.
    pub read_fn: Option<MpvStreamCbReadFn>,
    /// Optional. Set to None if the stream is not seekable.
    pub seek_fn: Option<MpvStreamCbSeekFn>,
    /// Optional. Set to None if total size is unknown.
    pub size_fn: Option<MpvStreamCbSizeFn>,
    /// Optional. Called when mpv closes the stream.
    pub close_fn: Option<MpvStreamCbCloseFn>,
    /// Optional. Cancel a blocking read (API >= 2.3).
    pub cancel_fn: Option<MpvStreamCbCancelFn>,
}

// ═══════════════════════════════════════════════════════════════════════════
// render.h — software frame extraction
//
// Lets us extract decoded video frames into a RAM buffer without GPU
// sharing. We create a render context with API type "sw" (software),
// then call mpv_render_context_render() to copy the current frame
// into our pixel buffer.
//
// Pixel format: "bgr0" = 4 bytes per pixel (B, G, R, unused).
// We set the unused byte to 255 to get BGRA for Servo's compositor.
//
// Maps to: VideoFrame extraction for display in the browser.
// ═══════════════════════════════════════════════════════════════════════════

/// Terminates a MpvRenderParam array (type = 0, data = NULL).
pub const MPV_RENDER_PARAM_INVALID: c_int = 0;
/// API type string. "sw" for software rendering.
pub const MPV_RENDER_PARAM_API_TYPE: c_int = 1;
/// Output size as [width, height] (c_int array). SW render only.
pub const MPV_RENDER_PARAM_SW_SIZE: c_int = 17;
/// Pixel format string (e.g. "bgr0"). SW render only.
pub const MPV_RENDER_PARAM_SW_FORMAT: c_int = 18;
/// Row stride in bytes (size_t pointer). SW render only.
pub const MPV_RENDER_PARAM_SW_STRIDE: c_int = 19;
/// Pointer to the pixel buffer. SW render only.
pub const MPV_RENDER_PARAM_SW_POINTER: c_int = 20;

/// Flag returned by mpv_render_context_update(): a new frame is ready.
pub const MPV_RENDER_UPDATE_FRAME: u64 = 1;

/// A type/data pair for passing render parameters.
/// Arrays of MpvRenderParam must be terminated with type_ = 0.
#[repr(C)]
pub struct MpvRenderParam {
    pub type_: c_int,
    pub data: *mut c_void,
}

/// Callback invoked by mpv when a new frame is available for rendering.
/// SAFETY: called from an mpv internal thread. Must not call any mpv API.
/// Signal your render thread and return immediately.
pub type MpvRenderUpdateFn = unsafe extern "C" fn(cb_ctx: *mut c_void);

// ═══════════════════════════════════════════════════════════════════════════
// Functions
//
// All functions below are unsafe C calls. The #[link(name = "mpv")] tells
// rustc to link this binary against libmpv.so (installed via system package).
// ═══════════════════════════════════════════════════════════════════════════

#[link(name = "mpv")]
unsafe extern "C" {

    // --- Lifecycle ---

    /// Return the client API version as (major << 16 | minor).
    pub fn mpv_client_api_version() -> u64;

    /// Allocate an uninitialized mpv handle. Must call mpv_initialize() next.
    pub fn mpv_create() -> *mut MpvHandle;

    /// Initialize the mpv instance. Options set before this call take effect.
    /// Returns 0 on success, negative mpv error code on failure.
    pub fn mpv_initialize(ctx: *mut MpvHandle) -> c_int;

    /// Destroy the mpv instance and free all resources.
    /// The handle is invalid after this call.
    pub fn mpv_terminate_destroy(ctx: *mut MpvHandle);

    // --- Configuration (before mpv_initialize) ---

    /// Set an option as a string. Only valid before mpv_initialize().
    /// Common options: "vo" (video output), "ao" (audio output).
    pub fn mpv_set_option_string(
        ctx: *mut MpvHandle,
        name: *const c_char,
        data: *const c_char,
    ) -> c_int;

    // --- Properties (after mpv_initialize) ---

    /// Set a property value. `format` indicates the type of `data`.
    /// Example: set "volume" with MPV_FORMAT_DOUBLE → data is *const f64.
    pub fn mpv_set_property(
        ctx: *mut MpvHandle,
        name: *const c_char,
        format: c_int,
        data: *const c_void,
    ) -> c_int;

    /// Read a property value into `data`. `format` indicates the expected type.
    pub fn mpv_get_property(
        ctx: *mut MpvHandle,
        name: *const c_char,
        format: c_int,
        data: *mut c_void,
    ) -> c_int;

    /// Set a property as a string (mpv parses it internally).
    pub fn mpv_set_property_string(
        ctx: *mut MpvHandle,
        name: *const c_char,
        data: *const c_char,
    ) -> c_int;

    /// Get a property as a string. Caller must free the result with mpv_free().
    pub fn mpv_get_property_string(
        ctx: *mut MpvHandle,
        name: *const c_char,
    ) -> *mut c_char;

    /// Free memory allocated by mpv (e.g. strings from mpv_get_property_string).
    pub fn mpv_free(data: *mut c_void);

    // --- Commands ---

    /// Send a command. `args` is a NULL-terminated array of C strings.
    /// Example: ["loadfile", "path.mp4", NULL] to start playback.
    pub fn mpv_command(ctx: *mut MpvHandle, args: *const *const c_char) -> c_int;

    // --- Events ---

    /// Subscribe to changes on a property. When it changes, mpv_wait_event()
    /// will return MPV_EVENT_PROPERTY_CHANGE with the given reply_userdata.
    pub fn mpv_observe_property(
        mpv: *mut MpvHandle,
        reply_userdata: u64,
        name: *const c_char,
        format: c_int,
    ) -> c_int;

    /// Wait up to `timeout` seconds for the next event. Returns a pointer to
    /// an MpvEvent owned by mpv — valid until the next call on this handle.
    /// Passing timeout=0 polls without blocking.
    pub fn mpv_wait_event(ctx: *mut MpvHandle, timeout: c_double) -> *const MpvEvent;

    // --- Error / debug ---

    /// Convert an mpv error code to a human-readable string.
    pub fn mpv_error_string(error: c_int) -> *const c_char;

    /// Convert an event ID to its name (e.g. 18 → "file-loaded").
    pub fn mpv_event_name(event: c_int) -> *const c_char;

    /// Enable log messages at or above `min_level` ("info", "v", "debug").
    /// They arrive as MPV_EVENT_LOG_MESSAGE events.
    pub fn mpv_request_log_messages(
        ctx: *mut MpvHandle,
        min_level: *const c_char,
    ) -> c_int;

    // --- stream_cb.h — custom protocol ---

    /// Register a read-only custom protocol. When mpv opens a URL starting
    /// with `protocol://`, it calls `open_fn` with `userdata`.
    /// Must be called before mpv_initialize().
    pub fn mpv_stream_cb_add_ro(
        ctx: *mut MpvHandle,
        protocol: *const c_char,
        userdata: *mut c_void,
        open_fn: MpvStreamCbOpenFn,
    ) -> c_int;

    // --- render.h — frame extraction ---

    /// Create a render context. `params` is a NULL-terminated MpvRenderParam array.
    /// For SW render: pass MPV_RENDER_PARAM_API_TYPE with value "sw".
    pub fn mpv_render_context_create(
        res: *mut *mut MpvRenderContext,
        mpv: *mut MpvHandle,
        params: *mut MpvRenderParam,
    ) -> c_int;

    /// Set a callback invoked when a new frame is available.
    /// The callback must not call any mpv function — just signal your thread.
    pub fn mpv_render_context_set_update_callback(
        ctx: *mut MpvRenderContext,
        callback: MpvRenderUpdateFn,
        callback_ctx: *mut c_void,
    );

    /// Check if rendering is needed. Returns a bitfield; test with
    /// MPV_RENDER_UPDATE_FRAME to know if a new frame is ready.
    pub fn mpv_render_context_update(ctx: *mut MpvRenderContext) -> u64;

    /// Render the current frame into the buffer described by `params`.
    /// `params` is a NULL-terminated MpvRenderParam array with SW_SIZE,
    /// SW_FORMAT, SW_STRIDE, and SW_POINTER.
    pub fn mpv_render_context_render(
        ctx: *mut MpvRenderContext,
        params: *mut MpvRenderParam,
    ) -> c_int;

    /// Destroy the render context. Must be called before mpv_terminate_destroy.
    pub fn mpv_render_context_free(ctx: *mut MpvRenderContext);
}
