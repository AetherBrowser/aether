/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::collections::HashMap;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, params};
use url::Url;

use crate::bookmarks::{add_bookmark, remove_bookmark};
use crate::history::{record_visit, replace_visit, set_title};

/// Schema migrations, in order. The database's `user_version` is the number of migrations
/// already applied, so existing entries must never change: append a new one instead.
pub(crate) const MIGRATIONS: &[&str] = &[
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
    // Version 3: the bookmarks, with the title of the page when it was added.
    "CREATE TABLE bookmarks (
        id INTEGER PRIMARY KEY,
        place_id INTEGER NOT NULL UNIQUE REFERENCES places(id),
        title TEXT,
        date_added INTEGER NOT NULL,
        guid TEXT NOT NULL UNIQUE
    );",
];

pub(crate) enum PlacesCommand {
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
    /// Bookmarks `url`, unless it already is.
    AddBookmark {
        url: Url,
        title: Option<String>,
        date_added: SystemTime,
    },
    /// Removes the bookmark of `url`.
    RemoveBookmark { url: Url },
}

/// The condition on `places` of a row that nothing refers to.
const UNUSED_PLACE: &str = "NOT EXISTS (SELECT 1 FROM visits WHERE visits.place_id = places.id)
    AND NOT EXISTS (SELECT 1 FROM bookmarks WHERE bookmarks.place_id = places.id)";

pub struct PlacesStore {
    pub(crate) connection: Connection,
}

impl PlacesStore {
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

    pub(crate) fn new(mut connection: Connection) -> rusqlite::Result<Self> {
        connection.pragma_update(None, "foreign_keys", true)?;
        connection.pragma_update(None, "secure_delete", true)?;
        migrate(&mut connection)?;
        Ok(Self { connection })
    }

    /// Writes `commands` in order, in one transaction. A title that a later command replaces
    /// is not written.
    pub(crate) fn apply(&mut self, commands: &[PlacesCommand]) -> rusqlite::Result<()> {
        let mut last_titles = HashMap::new();
        for (index, command) in commands.iter().enumerate() {
            if let PlacesCommand::SetTitle { url, .. } = command {
                last_titles.insert(url, index);
            }
        }
        let transaction = self.connection.transaction()?;
        for (index, command) in commands.iter().enumerate() {
            match command {
                PlacesCommand::RecordVisit {
                    url,
                    visit_date,
                    title,
                } => record_visit(&transaction, url, *visit_date, title.as_deref())?,
                PlacesCommand::SetTitle { url, title } => {
                    if last_titles[url] == index {
                        set_title(&transaction, url, title)?
                    }
                },
                PlacesCommand::ReplaceVisit {
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
                PlacesCommand::AddBookmark {
                    url,
                    title,
                    date_added,
                } => add_bookmark(&transaction, url, title.as_deref(), *date_added)?,
                PlacesCommand::RemoveBookmark { url } => remove_bookmark(&transaction, url)?,
            }
        }
        transaction.commit()?;
        if commands
            .iter()
            .any(|command| matches!(command, PlacesCommand::RemoveBookmark { .. }))
        {
            self.erase_deleted_rows()?;
        }
        Ok(())
    }

    /// Erases the deleted rows from the database files, so that they cannot be read back.
    pub(crate) fn erase_deleted_rows(&self) -> rusqlite::Result<()> {
        self.connection
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
    }
}

/// Adds `url` to the places if needed, and updates its title when `title` is known.
pub(crate) fn insert_place(
    connection: &Connection,
    url: &str,
    title: Option<&str>,
) -> rusqlite::Result<()> {
    connection.execute(
        "INSERT INTO places (url, guid, title) VALUES (?1, lower(hex(randomblob(16))), ?2)
         ON CONFLICT (url) DO UPDATE SET title = coalesce(excluded.title, places.title)",
        params![url, title],
    )?;
    Ok(())
}

pub(crate) fn stored_url(url: &Url) -> String {
    let mut url = url.clone();
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.into()
}

/// Deletes the places that nothing refers to.
pub(crate) fn delete_unused_places(connection: &Connection) -> rusqlite::Result<()> {
    connection.execute(&format!("DELETE FROM places WHERE {UNUSED_PLACE}"), [])?;
    Ok(())
}

/// Deletes the place of `url` if nothing refers to it.
pub(crate) fn delete_place_if_unused(connection: &Connection, url: &str) -> rusqlite::Result<()> {
    connection.execute(
        &format!("DELETE FROM places WHERE url = ?1 AND {UNUSED_PLACE}"),
        [url],
    )?;
    Ok(())
}

/// Whether the files of the database in `directory` contain `text`.
#[cfg(test)]
pub(crate) fn files_contain(directory: &Path, text: &str) -> bool {
    ["places.sqlite", "places.sqlite-wal"].iter().any(|file| {
        std::fs::read(directory.join(file))
            .unwrap_or_default()
            .windows(text.len())
            .any(|bytes| bytes == text.as_bytes())
    })
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
pub(crate) fn microseconds_since_epoch(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_micros() as i64)
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use super::{MIGRATIONS, PlacesStore};

    #[test]
    fn a_schema_newer_than_the_migrations_is_rejected() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .pragma_update(None, "user_version", MIGRATIONS.len() as u32 + 1)
            .unwrap();

        assert!(PlacesStore::new(connection).is_err());
    }
}
