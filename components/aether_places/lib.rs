/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! The places database of an Aether profile, stored in SQLite.
//!
//! The database follows the model of Firefox's `places.sqlite`: `places` lists each URL once,
//! `visits` has one row per visit, dated in microseconds since the Unix epoch (UTC), and
//! `bookmarks` has one row per bookmarked place. To read the visits by hand:
//!
//! ```sh
//! sqlite3 places.sqlite "SELECT datetime(visit_date / 1000000, 'unixepoch', 'localtime'), url
//!     FROM visits JOIN places ON places.id = visits.place_id ORDER BY visit_date"
//! ```

mod bookmarks;
mod database;
mod history;
mod service;

pub use database::PlacesStore;
pub use service::{CleanerReply, HistoryCleaner, PlacesService};
