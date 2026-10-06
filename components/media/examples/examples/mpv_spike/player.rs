/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use libmpv2::{GetData, Mpv, SetData};

use crate::{Result, describe};

pub(crate) struct MpvPlayer<'a> {
    pub(crate) mpv: &'a Mpv,
}

impl MpvPlayer<'_> {
    pub(crate) fn play(&self) -> Result<()> {
        self.set("pause", false)
    }

    pub(crate) fn pause(&self) -> Result<()> {
        self.set("pause", true)
    }

    pub(crate) fn paused(&self) -> Result<bool> {
        self.get("pause")
    }

    pub(crate) fn stop(&self) -> Result<()> {
        self.command("stop", &[])
    }

    pub(crate) fn seek(&self, time: f64) -> Result<()> {
        self.command("seek", &[&time.to_string(), "absolute"])
    }

    pub(crate) fn set_mute(&self, muted: bool) -> Result<()> {
        self.set("mute", muted)
    }

    pub(crate) fn muted(&self) -> Result<bool> {
        self.get("mute")
    }

    pub(crate) fn set_volume(&self, volume: f64) -> Result<()> {
        self.set("volume", volume * 100.0)
    }

    pub(crate) fn volume(&self) -> Result<f64> {
        Ok(self.get::<f64>("volume")? / 100.0)
    }

    pub(crate) fn set_playback_rate(&self, playback_rate: f64) -> Result<()> {
        self.set("speed", playback_rate)
    }

    pub(crate) fn playback_rate(&self) -> Result<f64> {
        self.get("speed")
    }

    pub(crate) fn command(&self, name: &str, args: &[&str]) -> Result<()> {
        self.mpv
            .command(name, args)
            .map_err(|err| format!("{name}: {}", describe(err)))
    }

    fn set<T: SetData>(&self, name: &str, value: T) -> Result<()> {
        self.mpv
            .set_property(name, value)
            .map_err(|err| format!("{name}: {}", describe(err)))
    }

    pub(crate) fn get<T: GetData>(&self, name: &str) -> Result<T> {
        self.mpv
            .get_property(name)
            .map_err(|err| format!("{name}: {}", describe(err)))
    }
}
