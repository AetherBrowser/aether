/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::collections::HashMap;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, params};
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

pub(crate) enum HistoryCommand {
    /// Records a visit of `url` at `visit_date`.
    RecordVisit {
        url: Url,
        visit_date: SystemTime,
        title: Option<String>,
    },
    /// Stores `title` as the last title of `url`, if `url` was visited.
    SetTitle { url: Url, title: String },
    /// Moves the visit of `old_url` at `visit_date` to `new_url`, or deletes it when `new_url`
    /// is `None`.
    ReplaceVisit {
        old_url: Url,
        visit_date: SystemTime,
        new_url: Option<Url>,
        title: Option<String>,
    },
}

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

    /// Writes `commands` in order, in one transaction. A title that a later command replaces
    /// is not written.
    pub(crate) fn apply(&mut self, commands: &[HistoryCommand]) -> rusqlite::Result<()> {
        let mut last_titles = HashMap::new();
        for (index, command) in commands.iter().enumerate() {
            if let HistoryCommand::SetTitle { url, .. } = command {
                last_titles.insert(url, index);
            }
        }
        let transaction = self.connection.transaction()?;
        for (index, command) in commands.iter().enumerate() {
            match command {
                HistoryCommand::RecordVisit {
                    url,
                    visit_date,
                    title,
                } => record_visit(&transaction, url, *visit_date, title.as_deref())?,
                HistoryCommand::SetTitle { url, title } => {
                    if last_titles[url] == index {
                        set_title(&transaction, url, title)?
                    }
                },
                HistoryCommand::ReplaceVisit {
                    old_url,
                    visit_date,
                    new_url,
                    title,
                } => replace_visit(
                    &transaction,
                    old_url,
                    *visit_date,
                    new_url.as_ref(),
                    title.as_deref(),
                )?,
            }
        }
        transaction.commit()
    }
}

fn record_visit(
    connection: &Connection,
    url: &Url,
    visit_date: SystemTime,
    title: Option<&str>,
) -> rusqlite::Result<()> {
    let url = stored_url(url);
    insert_place(connection, &url, title)?;
    connection.execute(
        "INSERT INTO visits (place_id, visit_date) SELECT id, ?2 FROM places WHERE url = ?1",
        params![url, microseconds_since_epoch(visit_date)],
    )?;
    Ok(())
}

fn set_title(connection: &Connection, url: &Url, title: &str) -> rusqlite::Result<()> {
    connection.execute(
        "UPDATE places SET title = ?2 WHERE url = ?1",
        params![stored_url(url), title],
    )?;
    Ok(())
}

fn replace_visit(
    connection: &Connection,
    old_url: &Url,
    visit_date: SystemTime,
    new_url: Option<&Url>,
    title: Option<&str>,
) -> rusqlite::Result<()> {
    let old_url = stored_url(old_url);
    let new_url = new_url.map(stored_url);
    let visit_date = microseconds_since_epoch(visit_date);
    if let Some(new_url) = &new_url {
        insert_place(connection, new_url, title)?;
    }
    let visit_id: Option<i64> = connection
        .query_row(
            "SELECT visits.id FROM visits JOIN places ON places.id = visits.place_id
             WHERE places.url = ?1 AND visits.visit_date = ?2
             ORDER BY visits.id DESC LIMIT 1",
            params![old_url, visit_date],
            |row| row.get(0),
        )
        .optional()?;
    match (visit_id, &new_url) {
        (Some(visit_id), Some(new_url)) => connection.execute(
            "UPDATE visits SET place_id = (SELECT id FROM places WHERE url = ?1) WHERE id = ?2",
            params![new_url, visit_id],
        )?,
        (Some(visit_id), None) => {
            connection.execute("DELETE FROM visits WHERE id = ?1", [visit_id])?
        },
        (None, Some(new_url)) => connection.execute(
            "INSERT INTO visits (place_id, visit_date) SELECT id, ?2 FROM places WHERE url = ?1",
            params![new_url, visit_date],
        )?,
        (None, None) => 0,
    };
    connection.execute(
        "DELETE FROM places WHERE url = ?1
         AND NOT EXISTS (SELECT 1 FROM visits WHERE visits.place_id = places.id)",
        [&old_url],
    )?;
    Ok(())
}

/// Adds `url` to the places if needed, and updates its title when `title` is known.
fn insert_place(connection: &Connection, url: &str, title: Option<&str>) -> rusqlite::Result<()> {
    connection.execute(
        "INSERT INTO places (url, guid, title) VALUES (?1, lower(hex(randomblob(16))), ?2)
         ON CONFLICT (url) DO UPDATE SET title = coalesce(excluded.title, places.title)",
        params![url, title],
    )?;
    Ok(())
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
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use rusqlite::Connection;
    use url::Url;

    use super::{HistoryCommand, HistoryStore, MIGRATIONS};

    impl HistoryStore {
        fn record_visit(
            &mut self,
            url: &Url,
            visit_date: SystemTime,
            title: Option<&str>,
        ) -> rusqlite::Result<()> {
            self.apply(&[HistoryCommand::RecordVisit {
                url: url.clone(),
                visit_date,
                title: title.map(str::to_owned),
            }])
        }

        fn set_title(&mut self, url: &Url, title: &str) -> rusqlite::Result<()> {
            self.apply(&[HistoryCommand::SetTitle {
                url: url.clone(),
                title: title.to_owned(),
            }])
        }

        fn replace_visit(
            &mut self,
            old_url: &Url,
            visit_date: SystemTime,
            new_url: Option<&Url>,
            title: Option<&str>,
        ) -> rusqlite::Result<()> {
            self.apply(&[HistoryCommand::ReplaceVisit {
                old_url: old_url.clone(),
                visit_date,
                new_url: new_url.cloned(),
                title: title.map(str::to_owned),
            }])
        }
    }

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
            .record_visit(&url, UNIX_EPOCH + Duration::from_micros(1_234_567), None)
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

        store.record_visit(&servo, UNIX_EPOCH, None).unwrap();
        store.record_visit(&rust, UNIX_EPOCH, None).unwrap();
        store.record_visit(&servo, UNIX_EPOCH, None).unwrap();

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
            .record_visit(&url, UNIX_EPOCH, None)
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
    fn visits_keep_the_last_known_title() {
        let mut store = HistoryStore::open_in_memory().unwrap();
        let url = Url::parse("https://servo.org/").unwrap();

        store.record_visit(&url, UNIX_EPOCH, Some("Servo")).unwrap();
        store.record_visit(&url, UNIX_EPOCH, None).unwrap();

        assert_eq!(
            titles(&store.connection),
            [("https://servo.org/".to_owned(), Some("Servo".to_owned()))]
        );
        assert_eq!(visits(&store.connection).len(), 2);
    }

    #[test]
    fn titles_of_urls_without_visit_are_not_stored() {
        let mut store = HistoryStore::open_in_memory().unwrap();

        store
            .set_title(&Url::parse("https://servo.org/").unwrap(), "Servo")
            .unwrap();

        assert!(titles(&store.connection).is_empty());
    }

    #[test]
    fn the_last_title_replaces_the_previous_one() {
        let mut store = HistoryStore::open_in_memory().unwrap();
        let url = Url::parse("https://servo.org/").unwrap();
        store.record_visit(&url, UNIX_EPOCH, Some("Servo")).unwrap();

        store.set_title(&url, "Servo blog").unwrap();

        assert_eq!(
            titles(&store.connection),
            [(
                "https://servo.org/".to_owned(),
                Some("Servo blog".to_owned())
            )]
        );
    }

    #[test]
    fn titles_replaced_in_the_same_batch_are_not_written() {
        let mut store = HistoryStore::open_in_memory().unwrap();
        let url = Url::parse("https://servo.org/").unwrap();
        let set_title = |title: &str| HistoryCommand::SetTitle {
            url: url.clone(),
            title: title.to_owned(),
        };
        let changes_before = store.connection.total_changes();

        store
            .apply(&[
                HistoryCommand::RecordVisit {
                    url: url.clone(),
                    visit_date: UNIX_EPOCH,
                    title: None,
                },
                set_title("Servo"),
                set_title("Servo blog"),
                set_title("Servo blog post"),
            ])
            .unwrap();

        assert_eq!(
            titles(&store.connection),
            [(
                "https://servo.org/".to_owned(),
                Some("Servo blog post".to_owned())
            )]
        );
        // The place, the visit and one title.
        assert_eq!(store.connection.total_changes() - changes_before, 3);
    }

    #[test]
    fn a_batch_is_written_in_order() {
        let mut store = HistoryStore::open_in_memory().unwrap();
        let redirect = Url::parse("https://duckduckgo.com/l/?uddg=wikipedia").unwrap();
        let target = Url::parse("https://www.wikipedia.org/").unwrap();
        let visit_date = UNIX_EPOCH + Duration::from_micros(42);

        store
            .apply(&[
                HistoryCommand::RecordVisit {
                    url: redirect.clone(),
                    visit_date,
                    title: None,
                },
                HistoryCommand::SetTitle {
                    url: redirect.clone(),
                    title: "DuckDuckGo".to_owned(),
                },
                HistoryCommand::ReplaceVisit {
                    old_url: redirect,
                    visit_date,
                    new_url: Some(target.clone()),
                    title: None,
                },
                HistoryCommand::SetTitle {
                    url: target,
                    title: "Wikipedia".to_owned(),
                },
            ])
            .unwrap();

        assert_eq!(
            visits(&store.connection),
            [("https://www.wikipedia.org/".to_owned(), 42)]
        );
        assert_eq!(
            titles(&store.connection),
            [(
                "https://www.wikipedia.org/".to_owned(),
                Some("Wikipedia".to_owned())
            )]
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
            .set_title(&Url::parse("https://servo.org/").unwrap(), "Servo")
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

        store
            .record_visit(&with_password, UNIX_EPOCH, None)
            .unwrap();
        store.set_title(&with_password, "Example").unwrap();
        store
            .record_visit(&username_only, UNIX_EPOCH, None)
            .unwrap();

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

    #[test]
    fn a_replaced_visit_moves_to_the_new_url_with_its_date() {
        let mut store = HistoryStore::open_in_memory().unwrap();
        let redirect = Url::parse("https://duckduckgo.com/l/?uddg=wikipedia").unwrap();
        let target = Url::parse("https://www.wikipedia.org/").unwrap();
        let visit_date = UNIX_EPOCH + Duration::from_micros(42);
        store
            .record_visit(&redirect, visit_date, Some("DuckDuckGo"))
            .unwrap();

        store
            .replace_visit(&redirect, visit_date, Some(&target), Some("Wikipedia"))
            .unwrap();

        assert_eq!(
            visits(&store.connection),
            [("https://www.wikipedia.org/".to_owned(), 42)]
        );
        assert_eq!(
            titles(&store.connection),
            [(
                "https://www.wikipedia.org/".to_owned(),
                Some("Wikipedia".to_owned())
            )]
        );
    }

    #[test]
    fn a_replaced_url_with_other_visits_is_kept() {
        let mut store = HistoryStore::open_in_memory().unwrap();
        let search = Url::parse("https://www.bing.com/search?q=hello").unwrap();
        let rewritten = Url::parse("https://www.bing.com/search?q=hello&rdr=1").unwrap();
        store.record_visit(&search, UNIX_EPOCH, None).unwrap();
        store
            .record_visit(&search, UNIX_EPOCH + Duration::from_micros(1), None)
            .unwrap();

        store
            .replace_visit(
                &search,
                UNIX_EPOCH + Duration::from_micros(1),
                Some(&rewritten),
                None,
            )
            .unwrap();

        assert_eq!(
            visits(&store.connection),
            [
                ("https://www.bing.com/search?q=hello".to_owned(), 0),
                ("https://www.bing.com/search?q=hello&rdr=1".to_owned(), 1),
            ]
        );
    }

    #[test]
    fn a_visit_replaced_by_an_unrecorded_page_is_deleted() {
        let mut store = HistoryStore::open_in_memory().unwrap();
        let url = Url::parse("https://servo.org/").unwrap();
        store.record_visit(&url, UNIX_EPOCH, None).unwrap();

        store.replace_visit(&url, UNIX_EPOCH, None, None).unwrap();

        assert!(visits(&store.connection).is_empty());
        assert!(titles(&store.connection).is_empty());
    }

    #[test]
    fn a_visit_that_was_not_written_is_given_to_the_new_url() {
        let mut store = HistoryStore::open_in_memory().unwrap();
        let old_url = Url::parse("https://servo.org/").unwrap();
        let new_url = Url::parse("https://servo.org/blog").unwrap();

        store
            .replace_visit(&old_url, UNIX_EPOCH, Some(&new_url), None)
            .unwrap();

        assert_eq!(
            visits(&store.connection),
            [("https://servo.org/blog".to_owned(), 0)]
        );
    }
}
