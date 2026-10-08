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
    // Version 2: the last title of each page, as shown in its tab.
    "ALTER TABLE places ADD COLUMN title TEXT;",
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
        let url = stored_url(url);
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "INSERT INTO places (url, guid) VALUES (?1, lower(hex(randomblob(16))))
             ON CONFLICT (url) DO NOTHING",
            [&url],
        )?;
        transaction.execute(
            "INSERT INTO visits (place_id, visit_date) SELECT id, ?2 FROM places WHERE url = ?1",
            params![url, microseconds_since_epoch(visit_date)],
        )?;
        transaction.commit()
    }

    /// Stores `title` as the last title of `url`, or `NULL` when it is missing or empty. The
    /// title can arrive before the first visit of `url`, which then reuses the same place.
    pub fn set_title(&mut self, url: &Url, title: Option<&str>) -> rusqlite::Result<()> {
        let title = title.filter(|title| !title.is_empty());
        self.connection.execute(
            "INSERT INTO places (url, guid, title) VALUES (?1, lower(hex(randomblob(16))), ?2)
             ON CONFLICT (url) DO UPDATE SET title = excluded.title",
            params![stored_url(url), title],
        )?;
        Ok(())
    }
}

fn stored_url(url: &Url) -> String {
    let mut url = url.clone();
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.into()
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

    fn titles(connection: &Connection) -> Vec<(String, Option<String>)> {
        connection
            .prepare("SELECT url, title FROM places ORDER BY id")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    #[test]
    fn titles_are_kept_whether_they_arrive_before_or_after_the_visit() {
        let mut store = HistoryStore::open_in_memory().unwrap();
        let servo = Url::parse("https://servo.org/").unwrap();
        let rust = Url::parse("https://www.rust-lang.org/").unwrap();

        store.set_title(&servo, Some("Servo")).unwrap();
        store.record_visit(&servo, UNIX_EPOCH).unwrap();
        store.record_visit(&rust, UNIX_EPOCH).unwrap();
        store.set_title(&rust, Some("Rust")).unwrap();

        assert_eq!(
            titles(&store.connection),
            [
                ("https://servo.org/".to_owned(), Some("Servo".to_owned())),
                (
                    "https://www.rust-lang.org/".to_owned(),
                    Some("Rust".to_owned())
                ),
            ]
        );
        assert_eq!(visits(&store.connection).len(), 2);
    }

    #[test]
    fn the_last_title_replaces_the_previous_one() {
        let mut store = HistoryStore::open_in_memory().unwrap();
        let url = Url::parse("https://servo.org/").unwrap();

        store.set_title(&url, Some("Servo")).unwrap();
        store.set_title(&url, Some("Servo blog")).unwrap();
        assert_eq!(
            titles(&store.connection),
            [(
                "https://servo.org/".to_owned(),
                Some("Servo blog".to_owned())
            )]
        );

        store.set_title(&url, Some("")).unwrap();
        assert_eq!(
            titles(&store.connection),
            [("https://servo.org/".to_owned(), None)]
        );
    }

    #[test]
    fn a_version_1_database_is_migrated_without_losing_visits() {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(MIGRATIONS[0]).unwrap();
        connection
            .execute_batch(
                "PRAGMA user_version = 1;
                 INSERT INTO places (id, url, guid) VALUES (1, 'https://servo.org/', 'guid');
                 INSERT INTO visits (place_id, visit_date) VALUES (1, 42);",
            )
            .unwrap();

        let mut store = HistoryStore::new(connection).unwrap();
        store
            .set_title(&Url::parse("https://servo.org/").unwrap(), Some("Servo"))
            .unwrap();

        assert_eq!(
            visits(&store.connection),
            [("https://servo.org/".to_owned(), 42)]
        );
        assert_eq!(
            titles(&store.connection),
            [("https://servo.org/".to_owned(), Some("Servo".to_owned()))]
        );
    }

    #[test]
    fn credentials_are_not_stored() {
        let mut store = HistoryStore::open_in_memory().unwrap();
        let with_password = Url::parse("https://user:secret@example.com/page").unwrap();
        let username_only = Url::parse("https://other@example.com/other").unwrap();

        store.record_visit(&with_password, UNIX_EPOCH).unwrap();
        store.set_title(&with_password, Some("Example")).unwrap();
        store.record_visit(&username_only, UNIX_EPOCH).unwrap();

        assert_eq!(
            visits(&store.connection),
            [
                ("https://example.com/page".to_owned(), 0),
                ("https://example.com/other".to_owned(), 0),
            ]
        );
        assert_eq!(
            titles(&store.connection),
            [
                (
                    "https://example.com/page".to_owned(),
                    Some("Example".to_owned())
                ),
                ("https://example.com/other".to_owned(), None),
            ]
        );

        let stored_urls: Vec<String> = store
            .connection
            .prepare("SELECT url FROM places")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(
            stored_urls
                .iter()
                .all(|url| !url.contains('@') && !url.contains("secret")),
            "{stored_urls:?}"
        );
    }
}
