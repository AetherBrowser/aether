/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Browsing history of an Aether profile, stored in a SQLite database.
//!
//! The database follows the model of Firefox's `places.sqlite`: `places` lists each URL once
//! and `visits` has one row per visit, dated in microseconds since the Unix epoch (UTC). To
//! read it by hand:
//!
//! ```sh
//! sqlite3 places.sqlite "SELECT datetime(visit_date / 1000000, 'unixepoch', 'localtime'), url
//!     FROM visits JOIN places ON places.id = visits.place_id ORDER BY visit_date"
//! ```

mod service;
mod store;

pub use service::HistoryService;
pub use store::HistoryStore;
