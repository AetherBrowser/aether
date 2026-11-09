/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::time::SystemTime;

use rusqlite::{Connection, OptionalExtension, params};
use url::Url;

use crate::database::{
    PlacesStore, delete_place_if_unused, delete_unused_places, insert_place,
    microseconds_since_epoch, stored_url,
};

/// The condition on `places` of a row without visits.
const NO_VISIT: &str = "NOT EXISTS (SELECT 1 FROM visits WHERE visits.place_id = places.id)";

impl PlacesStore {
    /// Deletes the visits before `date`, and the places that nothing refers to anymore.
    pub(crate) fn delete_visits_before(&mut self, date: SystemTime) -> rusqlite::Result<()> {
        self.delete_visits("DELETE FROM visits WHERE visit_date < ?1", date)
    }

    /// Deletes the visits at or after `date`, and the places that nothing refers to anymore.
    pub(crate) fn delete_visits_since(&mut self, date: SystemTime) -> rusqlite::Result<()> {
        self.delete_visits("DELETE FROM visits WHERE visit_date >= ?1", date)
    }

    /// Also erases the deleted rows from the database files, so that they cannot be read back.
    fn delete_visits(&mut self, statement: &str, date: SystemTime) -> rusqlite::Result<()> {
        let transaction = self.connection.transaction()?;
        let deleted = transaction.execute(statement, [microseconds_since_epoch(date)])?;
        if deleted == 0 {
            return Ok(());
        }
        delete_unused_places(&transaction)?;
        transaction.execute(
            &format!("UPDATE places SET title = NULL WHERE title IS NOT NULL AND {NO_VISIT}"),
            [],
        )?;
        transaction.commit()?;
        self.erase_deleted_rows()
    }
}

pub(crate) fn record_visit(
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

pub(crate) fn set_title(connection: &Connection, url: &Url, title: &str) -> rusqlite::Result<()> {
    connection.execute(
        "UPDATE places SET title = ?2 WHERE url = ?1
         AND EXISTS (SELECT 1 FROM visits WHERE visits.place_id = places.id)",
        params![stored_url(url), title],
    )?;
    Ok(())
}

pub(crate) fn replace_visit(
    connection: &Connection,
    old_url: &Url,
    visit_date: SystemTime,
    new_url: Option<&Url>,
    title: Option<&str>,
) -> rusqlite::Result<()> {
    let old_url = stored_url(old_url);
    let new_url = new_url.map(stored_url);
    let visit_date = microseconds_since_epoch(visit_date);
    let visit_id: Option<i64> = connection
        .query_row(
            "SELECT visits.id FROM visits JOIN places ON places.id = visits.place_id
             WHERE places.url = ?1 AND visits.visit_date = ?2
             ORDER BY visits.id DESC LIMIT 1",
            params![old_url, visit_date],
            |row| row.get(0),
        )
        .optional()?;
    let Some(visit_id) = visit_id else {
        return Ok(());
    };
    match &new_url {
        Some(new_url) => {
            insert_place(connection, new_url, title)?;
            connection.execute(
                "UPDATE visits SET place_id = (SELECT id FROM places WHERE url = ?1) WHERE id = ?2",
                params![new_url, visit_id],
            )?
        },
        None => connection.execute("DELETE FROM visits WHERE id = ?1", [visit_id])?,
    };
    delete_place_if_unused(connection, &old_url)?;
    connection.execute(
        &format!("UPDATE places SET title = NULL WHERE url = ?1 AND {NO_VISIT}"),
        [&old_url],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use rusqlite::Connection;
    use url::Url;

    use crate::database::{MIGRATIONS, PlacesCommand, PlacesStore, files_contain};

    impl PlacesStore {
        fn record_visit(
            &mut self,
            url: &Url,
            visit_date: SystemTime,
            title: Option<&str>,
        ) -> rusqlite::Result<()> {
            self.apply(&[PlacesCommand::RecordVisit {
                url: url.clone(),
                visit_date,
                title: title.map(str::to_owned),
            }])
        }

        fn set_title(&mut self, url: &Url, title: &str) -> rusqlite::Result<()> {
            self.apply(&[PlacesCommand::SetTitle {
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
            self.apply(&[PlacesCommand::ReplaceVisit {
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
        let mut store = PlacesStore::open_in_memory().unwrap();
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
        let mut store = PlacesStore::open_in_memory().unwrap();
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
        PlacesStore::open(&path)
            .unwrap()
            .record_visit(&url, UNIX_EPOCH, None)
            .unwrap();

        // The second opening must not apply the migrations again.
        let store = PlacesStore::open(&path).unwrap();

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
        let mut store = PlacesStore::open_in_memory().unwrap();
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
        let mut store = PlacesStore::open_in_memory().unwrap();

        store
            .set_title(&Url::parse("https://servo.org/").unwrap(), "Servo")
            .unwrap();

        assert!(titles(&store.connection).is_empty());
    }

    #[test]
    fn the_last_title_replaces_the_previous_one() {
        let mut store = PlacesStore::open_in_memory().unwrap();
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
        let mut store = PlacesStore::open_in_memory().unwrap();
        let url = Url::parse("https://servo.org/").unwrap();
        let set_title = |title: &str| PlacesCommand::SetTitle {
            url: url.clone(),
            title: title.to_owned(),
        };
        let changes_before = store.connection.total_changes();

        store
            .apply(&[
                PlacesCommand::RecordVisit {
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
        let mut store = PlacesStore::open_in_memory().unwrap();
        let redirect = Url::parse("https://duckduckgo.com/l/?uddg=wikipedia").unwrap();
        let target = Url::parse("https://www.wikipedia.org/").unwrap();
        let visit_date = UNIX_EPOCH + Duration::from_micros(42);

        store
            .apply(&[
                PlacesCommand::RecordVisit {
                    url: redirect.clone(),
                    visit_date,
                    title: None,
                },
                PlacesCommand::SetTitle {
                    url: redirect.clone(),
                    title: "DuckDuckGo".to_owned(),
                },
                PlacesCommand::ReplaceVisit {
                    old_url: redirect,
                    visit_date,
                    new_url: Some(target.clone()),
                    title: None,
                },
                PlacesCommand::SetTitle {
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

        let mut store = PlacesStore::new(connection).unwrap();
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
        let mut store = PlacesStore::open_in_memory().unwrap();
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
        let mut store = PlacesStore::open_in_memory().unwrap();
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
        let mut store = PlacesStore::open_in_memory().unwrap();
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
        let mut store = PlacesStore::open_in_memory().unwrap();
        let url = Url::parse("https://servo.org/").unwrap();
        store.record_visit(&url, UNIX_EPOCH, None).unwrap();

        store.replace_visit(&url, UNIX_EPOCH, None, None).unwrap();

        assert!(visits(&store.connection).is_empty());
        assert!(titles(&store.connection).is_empty());
    }

    #[test]
    fn visits_before_a_date_are_deleted_with_their_places() {
        let mut store = PlacesStore::open_in_memory().unwrap();
        let first = Url::parse("https://example.com/first").unwrap();
        let second = Url::parse("https://example.com/second").unwrap();
        let date = |micros| UNIX_EPOCH + Duration::from_micros(micros);
        store.record_visit(&first, date(1), Some("First")).unwrap();
        store
            .record_visit(&second, date(2), Some("Second"))
            .unwrap();
        store.record_visit(&first, date(3), None).unwrap();

        store.delete_visits_before(date(3)).unwrap();

        assert_eq!(
            visits(&store.connection),
            [("https://example.com/first".to_owned(), 3)]
        );
        assert_eq!(
            titles(&store.connection),
            [(
                "https://example.com/first".to_owned(),
                Some("First".to_owned())
            )]
        );
    }

    #[test]
    fn visits_since_a_date_are_deleted_with_their_places() {
        let mut store = PlacesStore::open_in_memory().unwrap();
        let first = Url::parse("https://example.com/first").unwrap();
        let second = Url::parse("https://example.com/second").unwrap();
        let date = |micros| UNIX_EPOCH + Duration::from_micros(micros);
        store.record_visit(&first, date(1), Some("First")).unwrap();
        store
            .record_visit(&second, date(2), Some("Second"))
            .unwrap();
        store.record_visit(&first, date(3), None).unwrap();

        store.delete_visits_since(date(2)).unwrap();

        assert_eq!(
            visits(&store.connection),
            [("https://example.com/first".to_owned(), 1)]
        );
        assert_eq!(
            titles(&store.connection),
            [(
                "https://example.com/first".to_owned(),
                Some("First".to_owned())
            )]
        );
    }

    #[test]
    fn visits_dated_before_the_epoch_are_deleted_with_all_visits() {
        let mut store = PlacesStore::open_in_memory().unwrap();
        let url = Url::parse("https://example.com/test").unwrap();
        store
            .record_visit(&url, UNIX_EPOCH - Duration::from_secs(1), None)
            .unwrap();

        store.delete_visits_since(UNIX_EPOCH).unwrap();

        assert!(visits(&store.connection).is_empty());
    }

    #[test]
    fn deleted_urls_and_titles_are_erased_from_the_files() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = PlacesStore::open(&directory.path().join("places.sqlite")).unwrap();
        let url = Url::parse("https://example.com/test").unwrap();
        store
            .record_visit(&url, UNIX_EPOCH, Some("Very secret title"))
            .unwrap();
        assert!(files_contain(directory.path(), "example.com/test"));

        store.delete_visits_since(UNIX_EPOCH).unwrap();

        assert!(!files_contain(directory.path(), "example.com/test"));
        assert!(!files_contain(directory.path(), "Very secret title"));
    }

    #[test]
    fn a_visit_that_no_longer_exists_is_not_recreated_by_its_replacement() {
        let mut store = PlacesStore::open_in_memory().unwrap();
        let old_url = Url::parse("https://example.com/test").unwrap();
        let new_url = Url::parse("https://example.com/new").unwrap();

        store
            .replace_visit(&old_url, UNIX_EPOCH, Some(&new_url), Some("New"))
            .unwrap();

        assert!(visits(&store.connection).is_empty());
        assert!(titles(&store.connection).is_empty());
    }
}
