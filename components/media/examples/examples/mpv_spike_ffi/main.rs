//! MPV backend spike — manual FFI approach.
//!
//! Validates that libmpv works for Aether's needs by exercising the raw C API
//! through our hand-written bindings in `ffi.rs`.

mod ffi;

use std::ffi::{CStr, CString};

fn main() {
    println!("=== MPV spike (manual FFI) ===\n");

    test_lifecycle();

    println!("\n=== All tests passed ===");
}

fn test_lifecycle() {
    println!("--- Lifecycle ---");

    unsafe {
        let version = ffi::mpv_client_api_version();
        let major = version >> 16;
        let minor = version & 0xFFFF;
        println!("[OK] API version: {}.{}", major, minor);

        let mpv = ffi::mpv_create();
        assert!(!mpv.is_null(), "mpv_create() returned NULL");
        println!("[OK] mpv_create()");

        let vo_key = CString::new("vo").unwrap();
        let ao_key = CString::new("ao").unwrap();
        let null_driver = CString::new("null").unwrap();

        let rc = ffi::mpv_set_option_string(mpv, vo_key.as_ptr(), null_driver.as_ptr());
        assert_eq!(rc, 0, "Failed to set vo=null");

        let rc = ffi::mpv_set_option_string(mpv, ao_key.as_ptr(), null_driver.as_ptr());
        assert_eq!(rc, 0, "Failed to set ao=null");

        let rc = ffi::mpv_initialize(mpv);
        assert_eq!(rc, 0, "mpv_initialize() failed");
        println!("[OK] mpv_initialize()");

        let ok_str = ffi::mpv_error_string(0);
        let ok_msg = CStr::from_ptr(ok_str).to_str().unwrap();
        println!("[OK] mpv_error_string(0) = \"{}\"", ok_msg);

        ffi::mpv_terminate_destroy(mpv);
        println!("[OK] mpv_terminate_destroy()");
    }
}
