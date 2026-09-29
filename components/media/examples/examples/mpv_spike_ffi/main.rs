//! MPV backend spike — manual FFI approach.
//!
//! Validates that libmpv works for Aether's needs by exercising the raw C API
//! through our hand-written bindings in `ffi.rs`.

mod ffi;

use std::ffi::{CStr, CString};
use std::os::raw::c_void;

fn main() {
    println!("=== MPV spike (manual FFI) ===\n");

    test_lifecycle();
    test_properties();

    println!("\n=== All tests passed ===");
}

unsafe fn create_mpv() -> *mut ffi::MpvHandle {
    unsafe {
        let mpv = ffi::mpv_create();
        assert!(!mpv.is_null(), "mpv_create() returned NULL");

        let vo_key = CString::new("vo").unwrap();
        let ao_key = CString::new("ao").unwrap();
        let null_driver = CString::new("null").unwrap();

        ffi::mpv_set_option_string(mpv, vo_key.as_ptr(), null_driver.as_ptr());
        ffi::mpv_set_option_string(mpv, ao_key.as_ptr(), null_driver.as_ptr());

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
