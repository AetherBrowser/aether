/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Safe wrapper around an `mpv_handle`. This is the only module allowed to call into `ffi`.

use std::ffi::{CStr, CString, c_int};
use std::mem::MaybeUninit;
use std::ptr::NonNull;
use std::{fmt, slice};

use crate::ffi;

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

/// An initialized mpv instance, destroyed on drop.
pub struct MpvHandle {
    ctx: NonNull<ffi::mpv_handle>,
}

// client.h: "The client API is generally fully thread-safe, unless otherwise noted."
// The only exception relevant here is mpv_wait_event(), which this wrapper does not expose.
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
        let handle = MpvHandle { ctx };
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

impl Drop for MpvHandle {
    fn drop(&mut self) {
        // Blocks until the core has shut down (audio output closed, threads joined).
        unsafe { ffi::mpv_terminate_destroy(self.ctx.as_ptr()) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn properties_are_read_as_owned_nodes() {
        let handle = MpvHandle::new(&[("ao", "null"), ("vid", "no")]).unwrap();
        let version = handle.get_node("mpv-version").unwrap();
        assert!(version.as_str().is_some_and(|version| !version.is_empty()));
        assert!(matches!(
            handle.get_node("not-a-property"),
            Err(MpvError::Api { .. })
        ));
    }
}
