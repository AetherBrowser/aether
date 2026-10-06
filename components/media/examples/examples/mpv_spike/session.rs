/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::cell::{Cell, Ref, RefCell};
use std::time::{Duration, Instant};

use libmpv2::events::{Event, PropertyData};
use libmpv2::{Mpv, mpv_log_level};

use crate::render::SwRenderer;
use crate::{Result, SEEK_TIMEOUT, describe};

pub(crate) struct Session<'a> {
    pub(crate) mpv: &'a Mpv,
    pub(crate) renderer: &'a SwRenderer<'a>,
    pub(crate) observed: RefCell<Observed>,
    pub(crate) log_messages: Cell<u64>,
}

/// Dernières valeurs reçues par PROPERTY_CHANGE.
#[derive(Default)]
pub(crate) struct Observed {
    pub(crate) pause: Option<bool>,
    pub(crate) time_pos: Option<f64>,
    pub(crate) duration: Option<f64>,
}

impl<'a> Session<'a> {
    pub(crate) fn new(mpv: &'a Mpv, renderer: &'a SwRenderer<'a>) -> Self {
        Self {
            mpv,
            renderer,
            observed: RefCell::default(),
            log_messages: Cell::new(0),
        }
    }

    pub(crate) fn observed(&self) -> Ref<'_, Observed> {
        self.observed.borrow()
    }

    pub(crate) fn wait_for(
        &self,
        what: &str,
        timeout: Duration,
        done: impl FnMut(&Event) -> bool,
    ) -> Result<()> {
        match self.drain(timeout, done) {
            Ok(true) => Ok(()),
            Ok(false) => Err(format!("{what}: pas de réponse après {timeout:?}")),
            Err(err) => Err(format!("{what}: {err}")),
        }
    }

    pub(crate) fn poll(
        &self,
        what: &str,
        timeout: Duration,
        mut condition: impl FnMut() -> Result<bool>,
    ) -> Result<()> {
        let deadline = Instant::now() + timeout;
        while !condition()? {
            if Instant::now() >= deadline {
                return Err(format!("{what}: pas de réponse après {timeout:?}"));
            }
            self.drain(Duration::from_millis(20), |_| false)
                .map_err(|err| format!("{what}: {err}"))?;
        }
        Ok(())
    }

    pub(crate) fn poll_pause(&self, paused: bool) -> Result<()> {
        self.poll("PROPERTY_CHANGE pause", SEEK_TIMEOUT, || {
            Ok(self.observed().pause == Some(paused))
        })
    }

    fn drain(&self, timeout: Duration, mut done: impl FnMut(&Event) -> bool) -> Result<bool> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            self.renderer.service()?;
            match self.mpv.wait_event(0.02) {
                None => {},
                Some(Ok(Event::LogMessage {
                    prefix,
                    level,
                    text,
                    log_level,
                })) => {
                    self.log_messages.set(self.log_messages.get() + 1);
                    if log_level <= mpv_log_level::Warn {
                        eprint!("    [mpv {level}] {prefix}: {text}");
                    }
                },
                Some(Ok(event)) => {
                    self.record(&event);
                    if done(&event) {
                        return Ok(true);
                    }
                },
                Some(Err(err)) => return Err(describe(err)),
            }
        }
        Ok(false)
    }

    fn record(&self, event: &Event) {
        let Event::PropertyChange { name, change, .. } = event else {
            return;
        };
        let mut observed = self.observed.borrow_mut();
        match (*name, change) {
            ("pause", PropertyData::Flag(paused)) => observed.pause = Some(*paused),
            ("time-pos", PropertyData::Double(time_pos)) => observed.time_pos = Some(*time_pos),
            ("duration", PropertyData::Double(duration)) => observed.duration = Some(*duration),
            _ => {},
        }
    }
}
