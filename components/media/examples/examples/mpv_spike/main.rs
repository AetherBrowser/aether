/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Spike hors navigateur : vérifie que libmpv (via `libmpv2` / `libmpv2-sys`)

mod player;
mod render;
mod report;
mod scenario;
mod session;
mod stream;

use std::ffi::c_int;
use std::mem;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use libmpv2::protocol::Protocol;
use libmpv2::{Format, Mpv};
use libmpv2_sys as sys;

use crate::render::SwRenderer;
use crate::report::Report;
use crate::session::Session;

pub(crate) const PROTOCOL: &str = "aether";

pub(crate) const LOAD_TIMEOUT: Duration = Duration::from_secs(10);
pub(crate) const SEEK_TIMEOUT: Duration = Duration::from_secs(5);
pub(crate) const FRAME_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) type Result<T> = std::result::Result<T, String>;

fn main() -> ExitCode {
    let mut files: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    if files.is_empty() {
        let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        files.push(crate_dir.join("examples/resources/mov_bbb.mp4"));
        files.push(crate_dir.join("../../../tests/wpt/tests/media/sine440.mp3"));
    }

    let mut report = Report::default();
    run(&files, &mut report);

    println!("\n{} OK, {} échec(s)", report.passed, report.failed);
    if report.failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn run(files: &[PathBuf], report: &mut Report) {
    println!("== lifecycle ==");
    let version = unsafe { sys::mpv_client_api_version() };
    let (major, minor) = (version >> 16, version & 0xffff);
    let api = if major >= 2 {
        Ok(format!("{major}.{minor}"))
    } else {
        Err(format!("{major}.{minor}, attendu >= 2.0"))
    };
    if !report.step("mpv_client_api_version", api) {
        return;
    }

    let mpv = match create_mpv() {
        Ok(mpv) => mpv,
        Err(err) => {
            report.step("mpv_create + mpv_initialize", Err(err));
            return;
        },
    };
    report.step("mpv_create + mpv_initialize", Ok("vo=libmpv".into()));

    let logs = check(
        unsafe { sys::mpv_request_log_messages(mpv.ctx.as_ptr(), c"info".as_ptr()) },
        "mpv_request_log_messages",
    )
    .map(|()| "niveau info".to_owned());
    report.step("mpv_request_log_messages", logs);

    let observed = [
        ("pause", Format::Flag),
        ("time-pos", Format::Double),
        ("duration", Format::Double),
    ]
    .into_iter()
    .zip(0..)
    .try_for_each(|((name, format), id)| mpv.observe_property(name, format, id))
    .map(|()| "pause, time-pos, duration".to_owned())
    .map_err(describe);
    report.step("mpv_observe_property", observed);

    let protocol = unsafe {
        Protocol::new(
            &mpv,
            PROTOCOL.into(),
            (),
            stream::open,
            stream::close,
            stream::read,
            Some(stream::seek),
            Some(stream::size),
        )
    };
    let registered = protocol
        .register()
        .map(|()| format!("{PROTOCOL}://"))
        .map_err(describe);
    if !report.step("mpv_stream_cb_add_ro", registered) {
        return;
    }

    let renderer = match SwRenderer::new(&mpv) {
        Ok(renderer) => renderer,
        Err(err) => {
            report.step("mpv_render_context_create", Err(err));
            return;
        },
    };
    report.step(
        "mpv_render_context_create",
        Ok("MPV_RENDER_API_TYPE_SW + update callback".into()),
    );

    let session = Session::new(&mpv, &renderer);
    for file in files {
        scenario::test_file(&session, file, report);
    }

    println!("\n== lifecycle ==");
    let logs = session.log_messages.get();
    let logs = if logs > 0 {
        Ok(format!("{logs} messages"))
    } else {
        Err("aucun message reçu".into())
    };
    report.step("LOG_MESSAGE reçus", logs);

    drop(session);
    drop(renderer);
    report.step("mpv_render_context_free", Ok(String::new()));
    drop(protocol);
    let handle = mpv.ctx.as_ptr();
    mem::forget(mpv);
    unsafe { sys::mpv_terminate_destroy(handle) };
    report.step("mpv_terminate_destroy", Ok(String::new()));
}

fn create_mpv() -> Result<Mpv> {
    Mpv::with_initializer(|init| init.set_option("vo", "libmpv"))
        .map_err(|err| format!("Mpv::with_initializer: {}", describe(err)))
}

pub(crate) fn describe(err: libmpv2::Error) -> String {
    match err {
        libmpv2::Error::Raw(code) => sys::mpv_error_str(code).to_owned(),
        other => format!("{other:?}"),
    }
}

pub(crate) fn check(code: c_int, what: &str) -> Result<()> {
    if code < 0 {
        Err(format!("{what}: {}", sys::mpv_error_str(code)))
    } else {
        Ok(())
    }
}
