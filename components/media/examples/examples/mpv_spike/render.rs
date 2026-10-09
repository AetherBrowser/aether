/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::cell::{Cell, RefCell};
use std::ffi::{c_int, c_void};
use std::fs;
use std::marker::PhantomData;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::{ptr, slice};

use libmpv2::Mpv;
use libmpv2_sys as sys;

use crate::{Result, check};

const FRAME_WIDTH: usize = 640;
const FRAME_HEIGHT: usize = 360;
const FRAME_STRIDE: usize = FRAME_WIDTH * 4;

static FRAME_READY: AtomicBool = AtomicBool::new(false);
pub(crate) static UPDATE_CALLBACKS: AtomicU64 = AtomicU64::new(0);

unsafe extern "C" fn on_render_update(_: *mut c_void) {
    UPDATE_CALLBACKS.fetch_add(1, Ordering::Relaxed);
    FRAME_READY.store(true, Ordering::Release);
}

pub(crate) struct SwRenderer<'a> {
    ctx: *mut sys::mpv_render_context,
    frame: RefCell<Frame>,
    pub(crate) frames_rendered: Cell<u64>,
    _mpv: PhantomData<&'a Mpv>,
}

impl<'a> SwRenderer<'a> {
    pub(crate) fn new(mpv: &'a Mpv) -> Result<Self> {
        let mut params = [
            render_param(
                sys::mpv_render_param_type_MPV_RENDER_PARAM_API_TYPE,
                sys::MPV_RENDER_API_TYPE_SW.as_ptr(),
            ),
            render_param(
                sys::mpv_render_param_type_MPV_RENDER_PARAM_INVALID,
                ptr::null::<c_void>(),
            ),
        ];
        let mut ctx = ptr::null_mut();
        check(
            unsafe {
                sys::mpv_render_context_create(&mut ctx, mpv.ctx.as_ptr(), params.as_mut_ptr())
            },
            "mpv_render_context_create",
        )?;
        unsafe {
            sys::mpv_render_context_set_update_callback(
                ctx,
                Some(on_render_update),
                ptr::null_mut(),
            )
        };
        Ok(Self {
            ctx,
            frame: RefCell::new(Frame::new()),
            frames_rendered: Cell::new(0),
            _mpv: PhantomData,
        })
    }

    pub(crate) fn reset(&self) {
        *self.frame.borrow_mut() = Frame::new();
        self.frames_rendered.set(0);
        UPDATE_CALLBACKS.store(0, Ordering::Relaxed);
    }

    pub(crate) fn service(&self) -> Result<()> {
        if !FRAME_READY.swap(false, Ordering::Acquire) {
            return Ok(());
        }
        let flags = unsafe { sys::mpv_render_context_update(self.ctx) };
        if flags & u64::from(sys::mpv_render_update_flag_MPV_RENDER_UPDATE_FRAME) != 0 {
            self.render()?;
            self.frames_rendered.set(self.frames_rendered.get() + 1);
        }
        Ok(())
    }

    fn render(&self) -> Result<()> {
        let mut frame = self.frame.borrow_mut();
        let size = [FRAME_WIDTH as c_int, FRAME_HEIGHT as c_int];
        let stride: usize = FRAME_STRIDE;
        let mut params = [
            render_param(
                sys::mpv_render_param_type_MPV_RENDER_PARAM_SW_SIZE,
                size.as_ptr(),
            ),
            render_param(
                sys::mpv_render_param_type_MPV_RENDER_PARAM_SW_FORMAT,
                c"rgb0".as_ptr(),
            ),
            render_param(
                sys::mpv_render_param_type_MPV_RENDER_PARAM_SW_STRIDE,
                ptr::from_ref(&stride),
            ),
            render_param(
                sys::mpv_render_param_type_MPV_RENDER_PARAM_SW_POINTER,
                frame.as_mut_ptr().cast_const(),
            ),
            render_param(
                sys::mpv_render_param_type_MPV_RENDER_PARAM_INVALID,
                ptr::null::<c_void>(),
            ),
        ];
        check(
            unsafe { sys::mpv_render_context_render(self.ctx, params.as_mut_ptr()) },
            "mpv_render_context_render",
        )
    }

    pub(crate) fn has_content(&self) -> bool {
        self.frame.borrow().has_content()
    }

    pub(crate) fn write_ppm(&self, path: &Path) -> Result<String> {
        let mut out = format!("P6\n{FRAME_WIDTH} {FRAME_HEIGHT}\n255\n").into_bytes();
        out.extend(
            self.frame
                .borrow()
                .pixels()
                .flat_map(|pixel| pixel[..3].iter().copied()),
        );
        fs::write(path, out).map_err(|err| format!("{}: {err}", path.display()))?;
        Ok(format!(
            "{FRAME_WIDTH}x{FRAME_HEIGHT} -> {}",
            path.display()
        ))
    }
}

impl Drop for SwRenderer<'_> {
    fn drop(&mut self) {
        unsafe { sys::mpv_render_context_free(self.ctx) };
    }
}

fn render_param<T>(type_: sys::mpv_render_param_type, data: *const T) -> sys::mpv_render_param {
    sys::mpv_render_param {
        type_,
        data: data as *mut c_void,
    }
}

#[derive(Clone, Copy)]
#[repr(C, align(64))]
struct Block([u8; 64]);

struct Frame(Vec<Block>);

impl Frame {
    fn new() -> Self {
        Frame(vec![Block([0; 64]); FRAME_STRIDE * FRAME_HEIGHT / 64])
    }

    fn as_mut_ptr(&mut self) -> *mut u8 {
        self.0.as_mut_ptr().cast()
    }

    fn pixels(&self) -> impl Iterator<Item = &[u8]> {
        let bytes =
            unsafe { slice::from_raw_parts(self.0.as_ptr().cast::<u8>(), self.0.len() * 64) };
        bytes.chunks_exact(4)
    }

    fn has_content(&self) -> bool {
        self.pixels()
            .any(|pixel| pixel[..3].iter().any(|&c| c > 16))
    }
}
