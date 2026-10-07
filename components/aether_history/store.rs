/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, params};
use url::Url;

/// Schema migrations, in order. The database's `user_version` is the number of migrations
/// already applied, so existing entries must never change: append a new one instead.
const MIGRATIONS: &[&str] = &[
    // Version 1: URLs and their visits. `places` will also be referenced by bookmarks, and its
    // `guid` identifies a URL across devices for a future sync.
    "CREATE TABLE places (
        id INTEGER PRIMARY KEY,
        url TEXT NOT NULL UNIQUE,
        guid TEXT NOT NULL UNIQUE
    );
    CREATE TABLE visits (
        id INTEGER PRIMARY KEY,
        place_id INTEGER NOT NULL REFERENCES places(id) ON DELETE CASCADE,
        visit_date INTEGER NOT NULL
    );
    CREATE INDEX visits_place_id_index ON visits(place_id);
    CREATE INDEX visits_date_index ON visits(visit_date);",
];

pub struct HistoryStore {
    connection: Connection,
}

impl HistoryStore {
    /// Opens the history database at `path`, creating it if needed.
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        let connection = Connection::open(path)?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "NORMAL")?;
        Self::new(connection)
    }

    /// Opens a history that is never written to disk.
    pub fn open_in_memory() -> rusqlite::Result<Self> {
        Self::new(Connection::open_in_memory()?)
    }

    fn new(mut connection: Connection) -> rusqlite::Result<Self> {
        connection.pragma_update(None, "foreign_keys", true)?;
        migrate(&mut connection)?;
        Ok(Self { connection })
    }

    /// Records a visit of `url` at `visit_date`.
    pub fn record_visit(&mut self, url: &Url, visit_date: SystemTime) -> rusqlite::Result<()> {
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "INSERT INTO places (url, guid) VALUES (?1, lower(hex(randomblob(16))))
             ON CONFLICT (url) DO NOTHING",
            [url.as_str()],
        )?;
        transaction.execute(
            "INSERT INTO visits (place_id, visit_date) SELECT id, ?2 FROM places WHERE url = ?1",
            params![url.as_str(), microseconds_since_epoch(visit_date)],
        )?;
        transaction.commit()
    }
}

fn migrate(connection: &mut Connection) -> rusqlite::Result<()> {
    let version: u32 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version as usize > MIGRATIONS.len() {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
            Some(format!(
                "schema version {version} is newer than the supported version {}",
                MIGRATIONS.len()
            )),
        ));
    }
    let transaction = connection.transaction()?;
    for (migration, new_version) in MIGRATIONS.iter().zip(1_u32..).skip(version as usize) {
        transaction.execute_batch(migration)?;
        transaction.pragma_update(None, "user_version", new_version)?;
    }
    transaction.commit()
}

/// Dates before the Unix epoch, which only a wrong system clock produces, are stored as 0.
fn microseconds_since_epoch(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_micros() as i64)
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use rusqlite::Connection;
    use url::Url;

    use super::{HistoryStore, MIGRATIONS};

    fn visits(connection: &Connection) -> Vec<(String, i64)> {
        connection
            .prepare(
                "SELECT url, visit_date FROM visits JOIN places ON places.id = visits.place_id
                 ORDER BY visits.id",
            )
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    #[test]
    fn records_visits_with_their_date() {
        let mut store = HistoryStore::open_in_memory().unwrap();
        let url = Url::parse("https://servo.org/").unwrap();

        store
            .record_visit(&url, UNIX_EPOCH + Duration::from_micros(1_234_567))
            .unwrap();

        assert_eq!(
            visits(&store.connection),
            [("https://servo.org/".to_owned(), 1_234_567)]
        );
    }

    #[test]
    fn visits_of_the_same_url_share_one_place() {
        let mut store = HistoryStore::open_in_memory().unwrap();
        let servo = Url::parse("https://servo.org/").unwrap();
        let rust = Url::parse("https://www.rust-lang.org/").unwrap();

        store.record_visit(&servo, UNIX_EPOCH).unwrap();
        store.record_visit(&rust, UNIX_EPOCH).unwrap();
        store.record_visit(&servo, UNIX_EPOCH).unwrap();

        let guids: Vec<String> = store
            .connection
            .prepare("SELECT guid FROM places")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(guids.len(), 2);
        assert_ne!(guids[0], guids[1]);
        assert!(guids.iter().all(|guid| guid.len() == 32));
        assert_eq!(visits(&store.connection).len(), 3);
    }

    #[test]
    fn reopening_the_database_keeps_the_history() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("places.sqlite");
        let url = Url::parse("https://servo.org/").unwrap();
        HistoryStore::open(&path)
            .unwrap()
            .record_visit(&url, UNIX_EPOCH)
            .unwrap();

        // The second opening must not apply the migrations again.
        let store = HistoryStore::open(&path).unwrap();

        let version: u32 = store
            .connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version as usize, MIGRATIONS.len());
        assert_eq!(
            visits(&store.connection),
            [("https://servo.org/".to_owned(), 0)]
        );
    }

    #[test]
    fn a_schema_newer_than_the_migrations_is_rejected() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .pragma_update(None, "user_version", MIGRATIONS.len() as u32 + 1)
            .unwrap();

        assert!(HistoryStore::new(connection).is_err());
    }
}
