/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::time::SystemTime;

use rusqlite::{Connection, params};
use url::Url;

use crate::database::{
    PlacesStore, delete_place_if_unused, insert_place, microseconds_since_epoch, stored_url,
};

impl PlacesStore {
    pub fn bookmarked_urls(&self) -> rusqlite::Result<Vec<Url>> {
        let mut statement = self
            .connection
            .prepare("SELECT url FROM places JOIN bookmarks ON bookmarks.place_id = places.id")?;
        let urls = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(urls.iter().filter_map(|url| Url::parse(url).ok()).collect())
    }
}

pub(crate) fn add_bookmark(
    connection: &Connection,
    url: &Url,
    title: Option<&str>,
    date_added: SystemTime,
) -> rusqlite::Result<()> {
    let url = stored_url(url);
    insert_place(connection, &url, None)?;
    connection.execute(
        "INSERT INTO bookmarks (place_id, title, date_added, guid)
         SELECT id, ?2, ?3, lower(hex(randomblob(16))) FROM places WHERE url = ?1
         ON CONFLICT (place_id) DO NOTHING",
        params![url, title, microseconds_since_epoch(date_added)],
    )?;
    Ok(())
}

pub(crate) fn remove_bookmark(connection: &Connection, url: &Url) -> rusqlite::Result<()> {
    let url = stored_url(url);
    connection.execute(
        "DELETE FROM bookmarks WHERE place_id = (SELECT id FROM places WHERE url = ?1)",
        [&url],
    )?;
    delete_place_if_unused(connection, &url)
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use rusqlite::Connection;
    use url::Url;

    use crate::database::{MIGRATIONS, PlacesCommand, PlacesStore, files_contain};

    fn url(url: &str) -> Url {
        Url::parse(url).unwrap()
    }

    fn date(micros: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_micros(micros)
    }

    fn add(store: &mut PlacesStore, url: &Url, title: Option<&str>, date_added: SystemTime) {
        store
            .apply(&[PlacesCommand::AddBookmark {
                url: url.clone(),
                title: title.map(str::to_owned),
                date_added,
            }])
            .unwrap();
    }

    fn remove(store: &mut PlacesStore, url: &Url) {
        store
            .apply(&[PlacesCommand::RemoveBookmark { url: url.clone() }])
            .unwrap();
    }

    fn visit(store: &mut PlacesStore, url: &Url, visit_date: SystemTime) {
        store
            .apply(&[PlacesCommand::RecordVisit {
                url: url.clone(),
                visit_date,
                title: None,
            }])
            .unwrap();
    }

    fn bookmarks(connection: &Connection) -> Vec<(String, Option<String>, i64)> {
        connection
            .prepare(
                "SELECT url, bookmarks.title, date_added
                 FROM bookmarks JOIN places ON places.id = bookmarks.place_id
                 ORDER BY bookmarks.id",
            )
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    fn places(connection: &Connection) -> Vec<String> {
        connection
            .prepare("SELECT url FROM places ORDER BY id")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    fn visit_count(connection: &Connection) -> u32 {
        connection
            .query_row("SELECT count(*) FROM visits", [], |row| row.get(0))
            .unwrap()
    }

    #[test]
    fn a_bookmark_keeps_its_title_and_date() {
        let mut store = PlacesStore::open_in_memory().unwrap();

        add(
            &mut store,
            &url("https://servo.org/"),
            Some("Servo"),
            date(42),
        );

        assert_eq!(
            bookmarks(&store.connection),
            [(
                "https://servo.org/".to_owned(),
                Some("Servo".to_owned()),
                42
            )]
        );
    }

    #[test]
    fn a_page_is_only_bookmarked_once() {
        let mut store = PlacesStore::open_in_memory().unwrap();
        let page = url("https://servo.org/");

        add(&mut store, &page, Some("First"), date(1));
        add(&mut store, &page, Some("Second"), date(2));

        assert_eq!(
            bookmarks(&store.connection),
            [("https://servo.org/".to_owned(), Some("First".to_owned()), 1)]
        );
    }

    #[test]
    fn credentials_are_not_stored_in_bookmarks() {
        let mut store = PlacesStore::open_in_memory().unwrap();

        add(
            &mut store,
            &url("https://user:secret@example.com/page"),
            None,
            date(1),
        );

        assert_eq!(places(&store.connection), ["https://example.com/page"]);
        remove(&mut store, &url("https://other@example.com/page"));
        assert!(bookmarks(&store.connection).is_empty());
    }

    #[test]
    fn removing_a_bookmark_deletes_a_place_without_visits() {
        let mut store = PlacesStore::open_in_memory().unwrap();
        let page = url("https://servo.org/");
        add(&mut store, &page, None, date(1));

        remove(&mut store, &page);

        assert!(bookmarks(&store.connection).is_empty());
        assert!(places(&store.connection).is_empty());
    }

    #[test]
    fn removing_a_bookmark_keeps_a_place_with_visits() {
        let mut store = PlacesStore::open_in_memory().unwrap();
        let page = url("https://servo.org/");
        visit(&mut store, &page, date(1));
        add(&mut store, &page, None, date(2));

        remove(&mut store, &page);

        assert!(bookmarks(&store.connection).is_empty());
        assert_eq!(places(&store.connection), ["https://servo.org/"]);
        assert_eq!(visit_count(&store.connection), 1);
    }

    #[test]
    fn bookmarks_are_kept_when_their_visits_expire() {
        let mut store = PlacesStore::open_in_memory().unwrap();
        let bookmarked = url("https://servo.org/");
        let other = url("https://example.com/");
        visit(&mut store, &bookmarked, date(1));
        visit(&mut store, &other, date(1));
        add(&mut store, &bookmarked, None, date(2));

        store.delete_visits_before(date(2)).unwrap();

        assert_eq!(visit_count(&store.connection), 0);
        assert_eq!(places(&store.connection), ["https://servo.org/"]);
        assert_eq!(bookmarks(&store.connection).len(), 1);
    }

    #[test]
    fn bookmarks_are_kept_when_the_history_is_deleted() {
        let mut store = PlacesStore::open_in_memory().unwrap();
        let bookmarked = url("https://servo.org/");
        let other = url("https://example.com/");
        visit(&mut store, &bookmarked, date(1));
        visit(&mut store, &other, date(1));
        add(&mut store, &bookmarked, None, date(2));

        store.delete_visits_since(UNIX_EPOCH).unwrap();

        assert_eq!(visit_count(&store.connection), 0);
        assert_eq!(places(&store.connection), ["https://servo.org/"]);
        assert_eq!(bookmarks(&store.connection).len(), 1);
    }

    #[test]
    fn deleting_the_history_erases_the_last_title_of_a_bookmarked_page() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = PlacesStore::open(&directory.path().join("places.sqlite")).unwrap();
        let page = url("https://example.com/test");
        store
            .apply(&[PlacesCommand::RecordVisit {
                url: page.clone(),
                visit_date: date(1),
                title: Some("Title of the visit".to_owned()),
            }])
            .unwrap();
        add(&mut store, &page, Some("Title of the bookmark"), date(2));

        store.delete_visits_since(UNIX_EPOCH).unwrap();

        assert!(!files_contain(directory.path(), "Title of the visit"));
        assert_eq!(
            bookmarks(&store.connection),
            [(
                "https://example.com/test".to_owned(),
                Some("Title of the bookmark".to_owned()),
                2
            )]
        );
    }

    fn place_titles(connection: &Connection) -> Vec<Option<String>> {
        connection
            .prepare("SELECT title FROM places ORDER BY id")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    fn visit_with_title(store: &mut PlacesStore, url: &Url, visit_date: SystemTime) {
        store
            .apply(&[PlacesCommand::RecordVisit {
                url: url.clone(),
                visit_date,
                title: Some("Title of the visit".to_owned()),
            }])
            .unwrap();
    }

    #[test]
    fn a_page_without_visits_does_not_store_its_new_title() {
        let mut store = PlacesStore::open_in_memory().unwrap();
        let page = url("https://servo.org/");
        visit_with_title(&mut store, &page, date(1));
        add(&mut store, &page, None, date(2));
        store.delete_visits_since(UNIX_EPOCH).unwrap();

        store
            .apply(&[PlacesCommand::SetTitle {
                url: page,
                title: "New title".to_owned(),
            }])
            .unwrap();

        assert_eq!(place_titles(&store.connection), [None]);
    }

    #[test]
    fn replacing_the_last_visit_clears_the_title_of_a_bookmarked_page() {
        for new_url in [None, Some(url("https://example.com/"))] {
            let mut store = PlacesStore::open_in_memory().unwrap();
            let page = url("https://servo.org/");
            visit_with_title(&mut store, &page, date(1));
            add(&mut store, &page, None, date(2));

            store
                .apply(&[PlacesCommand::ReplaceVisit {
                    old_url: page,
                    visit_date: date(1),
                    new_url,
                    title: None,
                }])
                .unwrap();

            assert_eq!(place_titles(&store.connection)[0], None);
        }
    }

    #[test]
    fn replacing_one_visit_keeps_the_title_of_a_page_with_other_visits() {
        let mut store = PlacesStore::open_in_memory().unwrap();
        let page = url("https://servo.org/");
        visit_with_title(&mut store, &page, date(1));
        visit_with_title(&mut store, &page, date(2));
        add(&mut store, &page, None, date(3));

        store
            .apply(&[PlacesCommand::ReplaceVisit {
                old_url: page,
                visit_date: date(2),
                new_url: None,
                title: None,
            }])
            .unwrap();

        assert_eq!(
            place_titles(&store.connection),
            [Some("Title of the visit".to_owned())]
        );
    }

    #[test]
    fn bookmarks_are_kept_when_their_visit_is_replaced() {
        let mut store = PlacesStore::open_in_memory().unwrap();
        let bookmarked = url("https://servo.org/");
        let target = url("https://example.com/");
        visit(&mut store, &bookmarked, date(1));
        add(&mut store, &bookmarked, None, date(2));

        store
            .apply(&[PlacesCommand::ReplaceVisit {
                old_url: bookmarked,
                visit_date: date(1),
                new_url: Some(target),
                title: None,
            }])
            .unwrap();

        assert_eq!(
            places(&store.connection),
            ["https://servo.org/", "https://example.com/"]
        );
        assert_eq!(bookmarks(&store.connection).len(), 1);
    }

    #[test]
    fn bookmarks_are_kept_when_the_database_is_reopened() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("places.sqlite");
        let page = url("https://servo.org/");
        add(&mut PlacesStore::open(&path).unwrap(), &page, None, date(1));

        let store = PlacesStore::open(&path).unwrap();

        assert_eq!(store.bookmarked_urls().unwrap(), [page]);
    }

    #[test]
    fn a_version_2_database_is_migrated_without_losing_its_history() {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(MIGRATIONS[0]).unwrap();
        connection.execute_batch(MIGRATIONS[1]).unwrap();
        connection
            .execute_batch(
                "PRAGMA user_version = 2;
                 INSERT INTO places (id, url, guid, title)
                     VALUES (1, 'https://servo.org/', 'guid', 'Servo');
                 INSERT INTO visits (place_id, visit_date) VALUES (1, 42);",
            )
            .unwrap();

        let mut store = PlacesStore::new(connection).unwrap();
        add(
            &mut store,
            &url("https://servo.org/"),
            Some("Bookmark"),
            date(1),
        );

        assert_eq!(places(&store.connection), ["https://servo.org/"]);
        assert_eq!(visit_count(&store.connection), 1);
        assert_eq!(bookmarks(&store.connection).len(), 1);
    }

    #[test]
    fn removed_bookmarks_are_erased_from_the_files() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = PlacesStore::open(&directory.path().join("places.sqlite")).unwrap();
        let page = url("https://example.com/test");
        add(&mut store, &page, Some("Very secret title"), date(1));
        assert!(files_contain(directory.path(), "example.com/test"));

        remove(&mut store, &page);

        assert!(!files_contain(directory.path(), "example.com/test"));
        assert!(!files_contain(directory.path(), "Very secret title"));
    }
}
