//! MPV backend spike — manual FFI approach.
//!
//! Validates that libmpv works for Aether's needs by exercising the raw C API
//! through our hand-written bindings in `ffi.rs`.

mod ffi;

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};
use std::ptr;

fn main() {
    println!("=== MPV spike (manual FFI) ===\n");

    test_lifecycle();
    test_properties();
    test_commands_and_events();
    test_stream_cb_callbacks();
    test_stream_cb();

    println!("\n=== All tests passed ===");
}

unsafe fn create_mpv() -> *mut ffi::MpvHandle {
    unsafe {
        let mpv = ffi::mpv_create();
        assert!(!mpv.is_null(), "mpv_create() returned NULL");

        let vo_key = CString::new("vo").unwrap();
        let ao_key = CString::new("ao").unwrap();
        let null_driver = CString::new("null").unwrap();

        let rc = ffi::mpv_set_option_string(mpv, vo_key.as_ptr(), null_driver.as_ptr());
        assert_eq!(rc, 0, "Failed to set vo=null");

        let rc = ffi::mpv_set_option_string(mpv, ao_key.as_ptr(), null_driver.as_ptr());
        assert_eq!(rc, 0, "Failed to set ao=null");

        let rc = ffi::mpv_initialize(mpv);
        assert_eq!(rc, 0, "mpv_initialize() failed");

        mpv
    }
}

fn test_lifecycle() {
    println!("--- Lifecycle ---");

    unsafe {
        let version = ffi::mpv_client_api_version();
        let major = version >> 16;
        let minor = version & 0xFFFF;
        println!("[OK] API version: {}.{}", major, minor);

        let mpv = create_mpv();
        println!("[OK] mpv_create() + mpv_initialize()");

        let ok_str = ffi::mpv_error_string(0);
        let ok_msg = CStr::from_ptr(ok_str).to_str().unwrap();
        println!("[OK] mpv_error_string(0) = \"{}\"", ok_msg);

        ffi::mpv_terminate_destroy(mpv);
        println!("[OK] mpv_terminate_destroy()");
    }
}

fn test_properties() {
    println!("\n--- Properties ---");

    unsafe {
        let mpv = create_mpv();

        // set_volume / volume (Player trait: set_volume, volume)
        let volume_key = CString::new("volume").unwrap();
        let mut volume: f64 = 75.0;
        let rc = ffi::mpv_set_property(
            mpv,
            volume_key.as_ptr(),
            ffi::MPV_FORMAT_DOUBLE,
            &volume as *const f64 as *const c_void,
        );
        assert_eq!(rc, 0, "Failed to set volume");

        volume = 0.0;
        let rc = ffi::mpv_get_property(
            mpv,
            volume_key.as_ptr(),
            ffi::MPV_FORMAT_DOUBLE,
            &mut volume as *mut f64 as *mut c_void,
        );
        assert_eq!(rc, 0, "Failed to get volume");
        assert!((volume - 75.0).abs() < 0.01, "Volume mismatch: {}", volume);
        println!("[OK] volume: set 75.0, got {}", volume);

        // set_mute / muted (Player trait: set_mute, muted)
        let mute_key = CString::new("mute").unwrap();
        let mut muted: i32 = 1;
        let rc = ffi::mpv_set_property(
            mpv,
            mute_key.as_ptr(),
            ffi::MPV_FORMAT_FLAG,
            &muted as *const i32 as *const c_void,
        );
        assert_eq!(rc, 0, "Failed to set mute");

        muted = 0;
        let rc = ffi::mpv_get_property(
            mpv,
            mute_key.as_ptr(),
            ffi::MPV_FORMAT_FLAG,
            &mut muted as *mut i32 as *mut c_void,
        );
        assert_eq!(rc, 0, "Failed to get mute");
        assert_eq!(muted, 1, "Mute should be true");
        println!("[OK] mute: set true, got {}", muted == 1);

        // set_playback_rate / playback_rate (Player trait: set_playback_rate, playback_rate)
        let speed_key = CString::new("speed").unwrap();
        let mut speed: f64 = 2.0;
        let rc = ffi::mpv_set_property(
            mpv,
            speed_key.as_ptr(),
            ffi::MPV_FORMAT_DOUBLE,
            &speed as *const f64 as *const c_void,
        );
        assert_eq!(rc, 0, "Failed to set speed");

        speed = 0.0;
        let rc = ffi::mpv_get_property(
            mpv,
            speed_key.as_ptr(),
            ffi::MPV_FORMAT_DOUBLE,
            &mut speed as *mut f64 as *mut c_void,
        );
        assert_eq!(rc, 0, "Failed to get speed");
        assert!((speed - 2.0).abs() < 0.01, "Speed mismatch: {}", speed);
        println!("[OK] speed: set 2.0, got {}", speed);

        // mpv_set_property_string / mpv_get_property_string
        let idle_key = CString::new("idle").unwrap();
        let idle_val = CString::new("yes").unwrap();
        let rc = ffi::mpv_set_property_string(mpv, idle_key.as_ptr(), idle_val.as_ptr());
        assert_eq!(rc, 0, "Failed to set idle");

        let result = ffi::mpv_get_property_string(mpv, idle_key.as_ptr());
        assert!(!result.is_null(), "get_property_string returned NULL");
        let result_str = CStr::from_ptr(result).to_str().unwrap();
        println!("[OK] idle: set \"yes\", got \"{}\"", result_str);
        ffi::mpv_free(result as *mut c_void);

        ffi::mpv_terminate_destroy(mpv);
        println!("[OK] properties cleanup");
    }
}

unsafe fn wait_for_event(mpv: *mut ffi::MpvHandle, target: i32, timeout: f64) -> bool {
    unsafe {
        let iterations = (timeout / 0.1) as i32;
        for _ in 0..iterations {
            let event = ffi::mpv_wait_event(mpv, 0.1);
            if (*event).event_id == ffi::MPV_EVENT_NONE {
                continue;
            }
            let name = CStr::from_ptr(ffi::mpv_event_name((*event).event_id));
            println!("  event: {}", name.to_str().unwrap());

            if (*event).event_id == target {
                return true;
            }
        }
        false
    }
}

unsafe fn drain_events(mpv: *mut ffi::MpvHandle, timeout: f64) {
    unsafe {
        let iterations = (timeout / 0.1) as i32;
        for _ in 0..iterations {
            let event = ffi::mpv_wait_event(mpv, 0.1);
            if (*event).event_id == ffi::MPV_EVENT_NONE {
                continue;
            }
            let name = CStr::from_ptr(ffi::mpv_event_name((*event).event_id));
            println!("  event: {}", name.to_str().unwrap());
        }
    }
}

fn test_commands_and_events() {
    println!("\n--- Commands & Events ---");

    unsafe {
        let mpv = create_mpv();

        let pause_key = CString::new("pause").unwrap();
        let rc = ffi::mpv_observe_property(mpv, 1, pause_key.as_ptr(), ffi::MPV_FORMAT_FLAG);
        assert_eq!(rc, 0, "Failed to observe pause");
        println!("[OK] mpv_observe_property(\"pause\")");

        // Load test.mp3
        let test_dir = env!("CARGO_MANIFEST_DIR");
        let test_path = format!("{}/examples/mpv_spike_ffi/test.mp3", test_dir);
        let loadfile = CString::new("loadfile").unwrap();
        let path_c = CString::new(test_path.as_str()).unwrap();
        let args: [*const i8; 3] = [loadfile.as_ptr(), path_c.as_ptr(), ptr::null()];
        let rc = ffi::mpv_command(mpv, args.as_ptr());
        assert_eq!(rc, 0, "mpv_command(loadfile) failed");
        println!("[OK] mpv_command([\"loadfile\", \"{}\"])", test_path);

        assert!(
            wait_for_event(mpv, ffi::MPV_EVENT_FILE_LOADED, 5.0),
            "Never received FILE_LOADED"
        );
        println!("[OK] received FILE_LOADED");

        // Pause
        let mut paused: i32 = 1;
        let rc = ffi::mpv_set_property(
            mpv,
            pause_key.as_ptr(),
            ffi::MPV_FORMAT_FLAG,
            &paused as *const i32 as *const c_void,
        );
        assert_eq!(rc, 0, "Failed to pause");
        println!("[OK] pause");

        drain_events(mpv, 2.0);

        // Resume
        paused = 0;
        let rc = ffi::mpv_set_property(
            mpv,
            pause_key.as_ptr(),
            ffi::MPV_FORMAT_FLAG,
            &paused as *const i32 as *const c_void,
        );
        assert_eq!(rc, 0, "Failed to resume");
        println!("[OK] resume");

        // Seek
        let seek_cmd = CString::new("seek").unwrap();
        let seek_pos = CString::new("0").unwrap();
        let seek_mode = CString::new("absolute").unwrap();
        let seek_args: [*const i8; 4] = [
            seek_cmd.as_ptr(),
            seek_pos.as_ptr(),
            seek_mode.as_ptr(),
            ptr::null(),
        ];
        let rc = ffi::mpv_command(mpv, seek_args.as_ptr());
        assert_eq!(rc, 0, "mpv_command(seek) failed");
        println!("[OK] seek to 0");

        drain_events(mpv, 2.0);

        // Stop
        let stop_cmd = CString::new("stop").unwrap();
        let stop_args: [*const i8; 2] = [stop_cmd.as_ptr(), ptr::null()];
        let rc = ffi::mpv_command(mpv, stop_args.as_ptr());
        assert_eq!(rc, 0, "mpv_command(stop) failed");
        println!("[OK] stop");

        ffi::mpv_terminate_destroy(mpv);
        println!("[OK] commands & events cleanup");
    }
}

// ── stream_cb: the data we pass to mpv through our custom protocol ──

struct StreamState {
    data: Vec<u8>,
    position: usize,
}

unsafe extern "C" fn stream_read(
    cookie: *mut c_void,
    buf: *mut c_char,
    nbytes: u64,
) -> i64 {
    unsafe {
        let state = &mut *(cookie as *mut StreamState);
        let remaining = state.data.len() - state.position;
        let to_read = std::cmp::min(nbytes as usize, remaining);

        if to_read == 0 {
            return 0;
        }

        std::ptr::copy_nonoverlapping(
            state.data[state.position..].as_ptr(),
            buf as *mut u8,
            to_read,
        );
        state.position += to_read;
        to_read as i64
    }
}

unsafe extern "C" fn stream_seek(cookie: *mut c_void, offset: i64) -> i64 {
    unsafe {
        let state = &mut *(cookie as *mut StreamState);
        if offset < 0 || offset as usize > state.data.len() {
            return -1;
        }
        state.position = offset as usize;
        state.position as i64
    }
}

unsafe extern "C" fn stream_size(cookie: *mut c_void) -> i64 {
    unsafe {
        let state = &*(cookie as *mut StreamState);
        state.data.len() as i64
    }
}

unsafe extern "C" fn stream_close(cookie: *mut c_void) {
    unsafe {
        let _ = Box::from_raw(cookie as *mut StreamState);
    }
}

unsafe extern "C" fn stream_open(
    _user_data: *mut c_void,
    _uri: *const c_char,
    info: *mut ffi::MpvStreamCbInfo,
) -> c_int {
    unsafe {
        let test_dir = env!("CARGO_MANIFEST_DIR");
        let test_path = format!("{}/examples/mpv_spike_ffi/test.mp3", test_dir);
        let data = std::fs::read(&test_path).expect("Failed to read test.mp3");

        let state = Box::new(StreamState { data, position: 0 });
        let cookie = Box::into_raw(state) as *mut c_void;

        (*info).cookie = cookie;
        (*info).read_fn = Some(stream_read);
        (*info).seek_fn = Some(stream_seek);
        (*info).size_fn = Some(stream_size);
        (*info).close_fn = Some(stream_close);

        0
    }
}

fn test_stream_cb_callbacks() {
    println!("\n--- stream_cb callbacks (edge cases) ---");

    let data = vec![10u8, 20, 30, 40, 50];
    let state = Box::new(StreamState { data, position: 0 });
    let cookie = Box::into_raw(state) as *mut c_void;

    unsafe {
        // Size
        assert_eq!(stream_size(cookie), 5);
        println!("[OK] size = 5");

        // Read all
        let mut buf = [0u8; 10];
        let n = stream_read(cookie, buf.as_mut_ptr() as *mut c_char, 10);
        assert_eq!(n, 5);
        assert_eq!(&buf[..5], &[10, 20, 30, 40, 50]);
        println!("[OK] read all: got 5 bytes");

        // Read at EOF
        let n = stream_read(cookie, buf.as_mut_ptr() as *mut c_char, 10);
        assert_eq!(n, 0);
        println!("[OK] read at EOF: got 0");

        // Seek to start
        assert_eq!(stream_seek(cookie, 0), 0);
        println!("[OK] seek to 0");

        // Read partial
        let n = stream_read(cookie, buf.as_mut_ptr() as *mut c_char, 3);
        assert_eq!(n, 3);
        assert_eq!(&buf[..3], &[10, 20, 30]);
        println!("[OK] read partial: got 3 bytes");

        // Seek to middle
        assert_eq!(stream_seek(cookie, 2), 2);
        let n = stream_read(cookie, buf.as_mut_ptr() as *mut c_char, 1);
        assert_eq!(n, 1);
        assert_eq!(buf[0], 30);
        println!("[OK] seek to 2, read 1 byte = 30");

        // Seek to end
        assert_eq!(stream_seek(cookie, 5), 5);
        let n = stream_read(cookie, buf.as_mut_ptr() as *mut c_char, 10);
        assert_eq!(n, 0);
        println!("[OK] seek to end, read = EOF");

        // Seek past end
        assert_eq!(stream_seek(cookie, 100), -1);
        println!("[OK] seek past end = -1");

        // Seek negative
        assert_eq!(stream_seek(cookie, -1), -1);
        println!("[OK] seek negative = -1");

        // Cleanup
        stream_close(cookie);
        println!("[OK] close");
    }
}

fn test_stream_cb() {
    println!("\n--- stream_cb (custom protocol) ---");

    unsafe {
        let mpv = ffi::mpv_create();
        assert!(!mpv.is_null(), "mpv_create() returned NULL");

        let vo_key = CString::new("vo").unwrap();
        let ao_key = CString::new("ao").unwrap();
        let null_driver = CString::new("null").unwrap();

        let rc = ffi::mpv_set_option_string(mpv, vo_key.as_ptr(), null_driver.as_ptr());
        assert_eq!(rc, 0, "Failed to set vo=null");
        let rc = ffi::mpv_set_option_string(mpv, ao_key.as_ptr(), null_driver.as_ptr());
        assert_eq!(rc, 0, "Failed to set ao=null");

        // Register custom protocol BEFORE mpv_initialize
        let protocol = CString::new("spike").unwrap();
        let rc = ffi::mpv_stream_cb_add_ro(
            mpv,
            protocol.as_ptr(),
            ptr::null_mut(),
            stream_open,
        );
        assert_eq!(rc, 0, "mpv_stream_cb_add_ro failed");
        println!("[OK] mpv_stream_cb_add_ro(\"spike\")");

        let rc = ffi::mpv_initialize(mpv);
        assert_eq!(rc, 0, "mpv_initialize() failed");

        // Load via our custom protocol
        let loadfile = CString::new("loadfile").unwrap();
        let url = CString::new("spike://test").unwrap();
        let args: [*const i8; 3] = [loadfile.as_ptr(), url.as_ptr(), ptr::null()];
        let rc = ffi::mpv_command(mpv, args.as_ptr());
        assert_eq!(rc, 0, "mpv_command(loadfile spike://test) failed");
        println!("[OK] mpv_command([\"loadfile\", \"spike://test\"])");

        assert!(
            wait_for_event(mpv, ffi::MPV_EVENT_FILE_LOADED, 5.0),
            "Never received FILE_LOADED via custom protocol"
        );
        println!("[OK] FILE_LOADED via custom protocol");

        // Pause
        let pause_key = CString::new("pause").unwrap();
        let mut paused: i32 = 1;
        let rc = ffi::mpv_set_property(
            mpv,
            pause_key.as_ptr(),
            ffi::MPV_FORMAT_FLAG,
            &paused as *const i32 as *const c_void,
        );
        assert_eq!(rc, 0, "Failed to pause");
        println!("[OK] pause via custom protocol");

        // Seek to 0 — mpv calls our stream_seek + stream_read
        let seek_cmd = CString::new("seek").unwrap();
        let seek_pos = CString::new("0").unwrap();
        let seek_mode = CString::new("absolute").unwrap();
        let seek_args: [*const i8; 4] = [
            seek_cmd.as_ptr(),
            seek_pos.as_ptr(),
            seek_mode.as_ptr(),
            ptr::null(),
        ];
        let rc = ffi::mpv_command(mpv, seek_args.as_ptr());
        assert_eq!(rc, 0, "seek failed via custom protocol");
        println!("[OK] seek to 0 via custom protocol");

        drain_events(mpv, 2.0);

        // Resume
        paused = 0;
        let rc = ffi::mpv_set_property(
            mpv,
            pause_key.as_ptr(),
            ffi::MPV_FORMAT_FLAG,
            &paused as *const i32 as *const c_void,
        );
        assert_eq!(rc, 0, "Failed to resume");
        println!("[OK] resume via custom protocol");

        // Let it play briefly then stop
        drain_events(mpv, 1.0);

        let stop_cmd = CString::new("stop").unwrap();
        let stop_args: [*const i8; 2] = [stop_cmd.as_ptr(), ptr::null()];
        let rc = ffi::mpv_command(mpv, stop_args.as_ptr());
        assert_eq!(rc, 0, "stop failed");
        println!("[OK] stop via custom protocol");

        ffi::mpv_terminate_destroy(mpv);
        println!("[OK] stream_cb cleanup");
    }
}
