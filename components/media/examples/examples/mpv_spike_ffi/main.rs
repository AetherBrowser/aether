//! MPV backend spike — manual FFI approach.
//!
//! Validates that libmpv works for Aether's needs by exercising the raw C API
//! through our hand-written bindings in `ffi.rs`.

mod ffi;

use std::ffi::{CStr, CString};
use std::os::raw::c_void;
use std::ptr;

fn main() {
    println!("=== MPV spike (manual FFI) ===\n");

    test_lifecycle();
    test_properties();
    test_commands_and_events();

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
