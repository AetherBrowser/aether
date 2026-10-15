/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Le scénario joué sur chaque fichier : une suite d'étapes qui donnent
//! chacune une ligne `[OK]` ou `[ECHEC]`.

use std::env;
use std::path::Path;
use std::sync::atomic::Ordering;

use libmpv2::events::Event;
use libmpv2::mpv_end_file_reason;

use crate::player::MpvPlayer;
use crate::render::UPDATE_CALLBACKS;
use crate::report::{Report, expect, expect_near};
use crate::session::Session;
use crate::stream::{BYTES_READ, STREAMS_OPENED};
use crate::{FRAME_TIMEOUT, LOAD_TIMEOUT, PROTOCOL, SEEK_TIMEOUT};

pub(crate) fn test_file(session: &Session, path: &Path, report: &mut Report) {
    let path = match path.canonicalize() {
        Ok(path) => path,
        Err(err) => {
            println!("\n== {} ==", path.display());
            report.step("fichier", Err(format!("{}: {err}", path.display())));
            return;
        },
    };
    println!("\n== {} ==", path.display());
    let player = MpvPlayer { mpv: session.mpv };
    session.observed.take();
    session.renderer.reset();
    STREAMS_OPENED.store(0, Ordering::Relaxed);
    BYTES_READ.store(0, Ordering::Relaxed);

    let url = format!("{PROTOCOL}://{}", path.display());
    let loaded = player
        .pause()
        .and_then(|()| player.command("loadfile", &[&url]))
        .and_then(|()| {
            session.wait_for("loadfile", LOAD_TIMEOUT, |event| {
                matches!(event, Event::FileLoaded)
            })
        })
        .and_then(|()| {
            let opened = STREAMS_OPENED.load(Ordering::Relaxed);
            let read = BYTES_READ.load(Ordering::Relaxed);
            if opened == 0 || read == 0 {
                return Err(format!("open_fn appelé {opened} fois, {read} octets lus"));
            }
            Ok(format!("FILE_LOADED, open_fn x{opened}, {read} octets lus"))
        });
    if !report.step("stream_cb + loadfile", loaded) {
        return;
    }

    let duration = session
        .poll("duration", SEEK_TIMEOUT, || {
            Ok(session.observed().duration.is_some())
        })
        .map(|()| {
            format!(
                "duration = {:.3}",
                session.observed().duration.unwrap_or_default()
            )
        });
    report.step("PROPERTY_CHANGE duration", duration);

    report.step(
        "play / paused",
        player
            .play()
            .and_then(|()| session.poll_pause(false))
            .and_then(|()| expect("paused()", player.paused()?, false)),
    );
    report.step(
        "pause / paused",
        player
            .pause()
            .and_then(|()| session.poll_pause(true))
            .and_then(|()| expect("paused()", player.paused()?, true)),
    );

    let seek = player.get::<f64>("duration").and_then(|duration| {
        let target = (duration / 2.0).floor();
        player.seek(target)?;
        let mut seeking = false;
        session.wait_for("seek", SEEK_TIMEOUT, |event| match event {
            Event::Seek => {
                seeking = true;
                false
            },
            Event::PlaybackRestart => seeking,
            _ => false,
        })?;
        session.poll("PROPERTY_CHANGE time-pos", SEEK_TIMEOUT, || {
            Ok(session
                .observed()
                .time_pos
                .is_some_and(|time_pos| (time_pos - target).abs() <= 0.5))
        })?;
        expect_near("time-pos", player.get::<f64>("time-pos")?, target, 0.5)
    });
    report.step("seek", seek);

    report.step(
        "set_volume / volume",
        player
            .set_volume(0.8)
            .and_then(|()| expect_near("volume()", player.volume()?, 0.8, 1e-6)),
    );
    report.step(
        "set_mute / muted",
        player
            .set_mute(true)
            .and_then(|()| expect("muted()", player.muted()?, true)),
    );
    let _ = player.set_mute(false);
    report.step(
        "set_playback_rate / playback_rate",
        player
            .set_playback_rate(1.5)
            .and_then(|()| expect_near("playback_rate()", player.playback_rate()?, 1.5, 1e-6)),
    );
    let _ = player.set_playback_rate(1.0);

    if player.get::<i64>("video-params/w").is_ok() {
        let stem = path.file_stem().unwrap_or_default().to_string_lossy();
        let out = env::temp_dir().join(format!("mpv_spike_{stem}.ppm"));
        let renderer = session.renderer;
        let frame = session
            .poll("frame SW", FRAME_TIMEOUT, || Ok(renderer.has_content()))
            .and_then(|()| renderer.write_ppm(&out))
            .map(|written| {
                format!(
                    "{} callbacks, {} frames rendues, {written}",
                    UPDATE_CALLBACKS.load(Ordering::Relaxed),
                    renderer.frames_rendered.get(),
                )
            });
        report.step("update callback + render SW", frame);
    } else {
        println!(
            "  [--]    {:<34} pas de piste vidéo",
            "update callback + render SW"
        );
    }

    let stop = player
        .stop()
        .and_then(|()| {
            session.wait_for("stop", LOAD_TIMEOUT, |event| {
                matches!(event, Event::EndFile(reason) if *reason == mpv_end_file_reason::Stop)
            })
        })
        .and_then(|()| {
            session.poll("idle-active", LOAD_TIMEOUT, || {
                player.get::<bool>("idle-active")
            })
        })
        .map(|()| "END_FILE (stop), idle-active = true".to_owned());
    report.step("stop", stop);

    let eof = player
        .command("loadfile", &[&url])
        .and_then(|()| {
            session.wait_for("loadfile", LOAD_TIMEOUT, |event| {
                matches!(event, Event::FileLoaded)
            })
        })
        .and_then(|()| {
            let duration = player.get::<f64>("duration")?;
            player.seek((duration - 1.0).max(0.0))?;
            player.play()?;
            session.wait_for("fin de lecture", LOAD_TIMEOUT, |event| {
                matches!(event, Event::EndFile(reason) if *reason == mpv_end_file_reason::Eof)
            })
        })
        .map(|()| "END_FILE (eof)".to_owned());
    report.step("lecture jusqu'à la fin", eof);
    let _ = player.pause();
}
