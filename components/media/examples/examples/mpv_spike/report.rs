/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Affichage des résultats et comparaisons utilisées par les étapes.

use std::fmt::Debug;

use crate::Result;

#[derive(Default)]
pub(crate) struct Report {
    pub(crate) passed: usize,
    pub(crate) failed: usize,
}

impl Report {
    pub(crate) fn step(&mut self, name: &str, result: Result<String>) -> bool {
        match result {
            Ok(detail) => {
                self.passed += 1;
                println!("  [OK]    {name:<34} {detail}");
                true
            },
            Err(err) => {
                self.failed += 1;
                println!("  [ECHEC] {name:<34} {err}");
                false
            },
        }
    }
}

pub(crate) fn expect<T: PartialEq + Debug>(what: &str, got: T, want: T) -> Result<String> {
    if got == want {
        Ok(format!("{what} = {got:?}"))
    } else {
        Err(format!("{what} = {got:?}, attendu {want:?}"))
    }
}

pub(crate) fn expect_near(what: &str, got: f64, want: f64, tolerance: f64) -> Result<String> {
    if (got - want).abs() <= tolerance {
        Ok(format!("{what} = {got:.3}"))
    } else {
        Err(format!("{what} = {got:.3}, attendu {want:.3}"))
    }
}
