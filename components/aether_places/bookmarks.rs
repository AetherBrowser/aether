/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::collections::HashMap;
use std::fmt;
use std::time::SystemTime;

use rusqlite::{Connection, OptionalExtension, params};
use url::Url;

use crate::database::{
    PlacesStore, delete_place_if_unused, delete_unused_places, insert_place,
    microseconds_since_epoch, stored_url,
};

/// A folder or a bookmark of the tree, with its children in their order.
#[derive(Clone, Debug, PartialEq)]
pub struct BookmarkNode {
    pub id: i64,
    /// The address of a bookmark, or `None` for a folder.
    pub url: Option<Url>,
    pub title: Option<String>,
    /// The PNG icon of the page.
    pub icon: Option<Vec<u8>>,
    pub children: Vec<BookmarkNode>,
}

/// A change of the tree. A `None` parent is the root, which is the bookmarks bar.
pub enum BookmarkEdit {
    CreateFolder {
        parent: Option<i64>,
        title: String,
    },
    /// Renames a folder or a bookmark, and changes the address of a bookmark when `url` is set.
    Update {
        id: i64,
        title: String,
        url: Option<Url>,
    },
    /// Moves a folder or a bookmark to `index` among the children of `parent`.
    Move {
        id: i64,
        parent: Option<i64>,
        index: usize,
    },
    /// Deletes a bookmark, or a folder with its content.
    Delete {
        id: i64,
    },
}

#[derive(Debug)]
pub enum BookmarkError {
    Database(rusqlite::Error),
    /// The edit cannot be done, with a message for the user.
    Invalid(&'static str),
}

impl From<rusqlite::Error> for BookmarkError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Database(error)
    }
}

impl fmt::Display for BookmarkError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Database(error) => write!(formatter, "{error}"),
            Self::Invalid(message) => formatter.write_str(message),
        }
    }
}

const NOT_FOUND: BookmarkError = BookmarkError::Invalid("This item no longer exists.");

impl PlacesStore {
    pub fn bookmark_tree(&self) -> rusqlite::Result<Vec<BookmarkNode>> {
        let mut statement = self.connection.prepare(
            "SELECT bookmarks.id, bookmarks.parent_id, places.url, bookmarks.title, bookmarks.icon
             FROM bookmarks LEFT JOIN places ON places.id = bookmarks.place_id
             ORDER BY bookmarks.position, bookmarks.id",
        )?;
        let rows = statement.query_map([], |row| {
            let url: Option<String> = row.get(2)?;
            let parent: Option<i64> = row.get(1)?;
            let node = BookmarkNode {
                id: row.get(0)?,
                url: url.as_deref().and_then(|url| Url::parse(url).ok()),
                title: row.get(3)?,
                icon: row.get(4)?,
                children: Vec::new(),
            };
            Ok((parent, node))
        })?;
        let mut children: HashMap<Option<i64>, Vec<BookmarkNode>> = HashMap::new();
        for row in rows {
            let (parent, node) = row?;
            children.entry(parent).or_default().push(node);
        }
        Ok(children_of(None, &mut children))
    }

    /// Applies `edit` at the date `now`, in one transaction.
    pub fn edit_bookmarks(
        &mut self,
        edit: &BookmarkEdit,
        now: SystemTime,
    ) -> Result<(), BookmarkError> {
        let transaction = self.connection.transaction()?;
        match edit {
            BookmarkEdit::CreateFolder { parent, title } => {
                create_folder(&transaction, *parent, title, now)?
            },
            BookmarkEdit::Update { id, title, url } => {
                update(&transaction, *id, title, url.as_ref())?
            },
            BookmarkEdit::Move { id, parent, index } => {
                move_item(&transaction, *id, *parent, *index)?
            },
            BookmarkEdit::Delete { id } => delete(&transaction, *id)?,
        }
        transaction.commit()?;
        if matches!(
            edit,
            BookmarkEdit::Update { .. } | BookmarkEdit::Delete { .. }
        ) {
            self.erase_deleted_rows()?;
        }
        Ok(())
    }
}

fn children_of(
    parent: Option<i64>,
    children: &mut HashMap<Option<i64>, Vec<BookmarkNode>>,
) -> Vec<BookmarkNode> {
    let mut nodes = children.remove(&parent).unwrap_or_default();
    for node in &mut nodes {
        node.children = children_of(Some(node.id), children);
    }
    nodes
}

/// The addresses of all the bookmarks of `nodes` and of their folders.
pub fn bookmarked_urls(nodes: &[BookmarkNode]) -> Vec<Url> {
    let mut urls = Vec::new();
    for node in nodes {
        urls.extend(node.url.clone());
        urls.extend(bookmarked_urls(&node.children));
    }
    urls
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
        "INSERT INTO bookmarks (place_id, position, title, date_added, guid)
         SELECT id, (SELECT coalesce(max(position) + 1, 0) FROM bookmarks WHERE parent_id IS NULL),
                ?2, ?3, lower(hex(randomblob(16)))
         FROM places WHERE url = ?1
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

/// Stores `icon` for the bookmark of `url`, and returns whether it was a new icon.
pub(crate) fn set_bookmark_icon(
    connection: &Connection,
    url: &Url,
    icon: &[u8],
) -> rusqlite::Result<bool> {
    let changed = connection.execute(
        "UPDATE bookmarks SET icon = ?2
         WHERE place_id = (SELECT id FROM places WHERE url = ?1) AND icon IS NOT ?2",
        params![stored_url(url), icon],
    )?;
    Ok(changed > 0)
}

/// The place of the item `id`, which is `None` for a folder.
fn place_of(connection: &Connection, id: i64) -> Result<Option<i64>, BookmarkError> {
    connection
        .query_row(
            "SELECT place_id FROM bookmarks WHERE id = ?1",
            [id],
            |row| row.get(0),
        )
        .optional()?
        .ok_or(NOT_FOUND)
}

fn check_folder(connection: &Connection, folder: Option<i64>) -> Result<(), BookmarkError> {
    match folder {
        Some(id) if place_of(connection, id)?.is_some() => Err(BookmarkError::Invalid(
            "A bookmark cannot contain other items.",
        )),
        _ => Ok(()),
    }
}

fn child_ids(connection: &Connection, parent: Option<i64>) -> rusqlite::Result<Vec<i64>> {
    connection
        .prepare("SELECT id FROM bookmarks WHERE parent_id IS ?1 ORDER BY position, id")?
        .query_map([parent], |row| row.get(0))?
        .collect()
}

fn write_order(connection: &Connection, ids: &[i64]) -> rusqlite::Result<()> {
    for (position, id) in ids.iter().enumerate() {
        connection.execute(
            "UPDATE bookmarks SET position = ?1 WHERE id = ?2",
            params![position as i64, id],
        )?;
    }
    Ok(())
}

fn create_folder(
    connection: &Connection,
    parent: Option<i64>,
    title: &str,
    now: SystemTime,
) -> Result<(), BookmarkError> {
    check_folder(connection, parent)?;
    let title = title.trim();
    if title.is_empty() {
        return Err(BookmarkError::Invalid("A folder needs a name."));
    }
    connection.execute(
        "INSERT INTO bookmarks (parent_id, position, title, date_added, guid)
         VALUES (?1, (SELECT coalesce(max(position) + 1, 0) FROM bookmarks WHERE parent_id IS ?1),
                 ?2, ?3, lower(hex(randomblob(16))))",
        params![parent, title, microseconds_since_epoch(now)],
    )?;
    Ok(())
}

fn update(
    connection: &Connection,
    id: i64,
    title: &str,
    url: Option<&Url>,
) -> Result<(), BookmarkError> {
    let title = title.trim();
    let Some(place) = place_of(connection, id)? else {
        if title.is_empty() {
            return Err(BookmarkError::Invalid("A folder needs a name."));
        }
        connection.execute(
            "UPDATE bookmarks SET title = ?2 WHERE id = ?1",
            params![id, title],
        )?;
        return Ok(());
    };
    connection.execute(
        "UPDATE bookmarks SET title = nullif(?2, '') WHERE id = ?1",
        params![id, title],
    )?;
    let Some(url) = url else {
        return Ok(());
    };
    let new_url = stored_url(url);
    let old_url: String =
        connection.query_row("SELECT url FROM places WHERE id = ?1", [place], |row| {
            row.get(0)
        })?;
    if new_url == old_url {
        return Ok(());
    }
    let already_bookmarked: bool = connection.query_row(
        "SELECT EXISTS (SELECT 1 FROM bookmarks JOIN places ON places.id = bookmarks.place_id
                        WHERE places.url = ?1)",
        [&new_url],
        |row| row.get(0),
    )?;
    if already_bookmarked {
        return Err(BookmarkError::Invalid(
            "This address is already bookmarked.",
        ));
    }
    insert_place(connection, &new_url, None)?;
    connection.execute(
        "UPDATE bookmarks SET icon = NULL, place_id = (SELECT id FROM places WHERE url = ?2)
         WHERE id = ?1",
        params![id, new_url],
    )?;
    delete_place_if_unused(connection, &old_url)?;
    Ok(())
}

fn move_item(
    connection: &Connection,
    id: i64,
    parent: Option<i64>,
    index: usize,
) -> Result<(), BookmarkError> {
    place_of(connection, id)?;
    check_folder(connection, parent)?;
    let mut ancestor = parent;
    while let Some(folder) = ancestor {
        if folder == id {
            return Err(BookmarkError::Invalid("A folder cannot go inside itself."));
        }
        ancestor = connection.query_row(
            "SELECT parent_id FROM bookmarks WHERE id = ?1",
            [folder],
            |row| row.get(0),
        )?;
    }
    let mut siblings = child_ids(connection, parent)?;
    siblings.retain(|sibling| *sibling != id);
    siblings.insert(index.min(siblings.len()), id);
    connection.execute(
        "UPDATE bookmarks SET parent_id = ?2 WHERE id = ?1",
        params![id, parent],
    )?;
    write_order(connection, &siblings)?;
    Ok(())
}

fn delete(connection: &Connection, id: i64) -> Result<(), BookmarkError> {
    place_of(connection, id)?;
    connection.execute("DELETE FROM bookmarks WHERE id = ?1", [id])?;
    delete_unused_places(connection)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use rusqlite::Connection;
    use url::Url;

    use super::{BookmarkEdit, BookmarkNode, bookmarked_urls};
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

        assert_eq!(bookmarked_urls(&store.bookmark_tree().unwrap()), [page]);
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

    /// The tree as one line per item, indented by depth. Folders end with `/`.
    fn outline(store: &PlacesStore) -> Vec<String> {
        fn visit_nodes(nodes: &[BookmarkNode], depth: usize, lines: &mut Vec<String>) {
            for node in nodes {
                let name = node
                    .title
                    .clone()
                    .or(node.url.as_ref().map(Url::to_string))
                    .unwrap_or_default();
                let marker = if node.url.is_none() { "/" } else { "" };
                lines.push(format!("{}{name}{marker}", "  ".repeat(depth)));
                visit_nodes(&node.children, depth + 1, lines);
            }
        }
        let mut lines = Vec::new();
        visit_nodes(&store.bookmark_tree().unwrap(), 0, &mut lines);
        lines
    }

    fn id_of(store: &PlacesStore, name: &str) -> i64 {
        fn find(nodes: &[BookmarkNode], name: &str) -> Option<i64> {
            nodes.iter().find_map(|node| {
                (node.title.as_deref() == Some(name))
                    .then_some(node.id)
                    .or_else(|| find(&node.children, name))
            })
        }
        find(&store.bookmark_tree().unwrap(), name).unwrap()
    }

    fn edit(store: &mut PlacesStore, edit: BookmarkEdit) {
        store.edit_bookmarks(&edit, date(100)).unwrap();
    }

    fn folder(store: &mut PlacesStore, parent: Option<i64>, title: &str) -> i64 {
        edit(
            store,
            BookmarkEdit::CreateFolder {
                parent,
                title: title.to_owned(),
            },
        );
        id_of(store, title)
    }

    fn bookmark(store: &mut PlacesStore, name: &str, added: u64) -> i64 {
        let page = url(&format!("https://{name}.example/"));
        add(store, &page, Some(name), date(added));
        id_of(store, name)
    }

    fn move_to(store: &mut PlacesStore, id: i64, parent: Option<i64>, index: usize) {
        edit(store, BookmarkEdit::Move { id, parent, index });
    }

    fn refused(store: &mut PlacesStore, change: BookmarkEdit) -> String {
        store
            .edit_bookmarks(&change, date(100))
            .unwrap_err()
            .to_string()
    }

    #[test]
    fn a_version_3_database_becomes_a_tree_ordered_by_date() {
        let connection = Connection::open_in_memory().unwrap();
        for migration in &MIGRATIONS[..3] {
            connection.execute_batch(migration).unwrap();
        }
        connection
            .execute_batch(
                "PRAGMA user_version = 3;
                 INSERT INTO places (id, url, guid) VALUES (1, 'https://b.example/', 'p1');
                 INSERT INTO places (id, url, guid) VALUES (2, 'https://a.example/', 'p2');
                 INSERT INTO bookmarks (id, place_id, title, date_added, guid)
                     VALUES (1, 1, 'Second', 20, 'b1');
                 INSERT INTO bookmarks (id, place_id, title, date_added, guid)
                     VALUES (2, 2, 'First', 10, 'b2');",
            )
            .unwrap();

        let mut store = PlacesStore::new(connection).unwrap();

        assert_eq!(outline(&store), ["First", "Second"]);
        let inner = folder(&mut store, None, "Folder");
        move_to(&mut store, 1, Some(inner), 0);
        edit(&mut store, BookmarkEdit::Delete { id: inner });
        assert_eq!(outline(&store), ["First"]);
    }

    #[test]
    fn new_bookmarks_go_at_the_end_of_the_root() {
        let mut store = PlacesStore::open_in_memory().unwrap();
        bookmark(&mut store, "first", 1);
        folder(&mut store, None, "Folder");

        bookmark(&mut store, "second", 2);

        assert_eq!(outline(&store), ["first", "Folder/", "second"]);
    }

    #[test]
    fn folders_can_be_nested_and_filled() {
        let mut store = PlacesStore::open_in_memory().unwrap();
        let page = bookmark(&mut store, "page", 1);
        let outer = folder(&mut store, None, "Outer");
        let inner = folder(&mut store, Some(outer), "Inner");

        move_to(&mut store, page, Some(inner), 0);

        assert_eq!(outline(&store), ["Outer/", "  Inner/", "    page"]);
    }

    #[test]
    fn moving_an_item_changes_its_place_among_its_siblings() {
        let mut store = PlacesStore::open_in_memory().unwrap();
        let first = bookmark(&mut store, "first", 1);
        bookmark(&mut store, "second", 2);
        let third = bookmark(&mut store, "third", 3);

        move_to(&mut store, third, None, 0);
        assert_eq!(outline(&store), ["third", "first", "second"]);

        move_to(&mut store, first, None, 99);
        assert_eq!(outline(&store), ["third", "second", "first"]);
    }

    #[test]
    fn a_folder_cannot_be_moved_inside_itself() {
        let mut store = PlacesStore::open_in_memory().unwrap();
        let outer = folder(&mut store, None, "Outer");
        let inner = folder(&mut store, Some(outer), "Inner");

        for parent in [outer, inner] {
            let message = refused(
                &mut store,
                BookmarkEdit::Move {
                    id: outer,
                    parent: Some(parent),
                    index: 0,
                },
            );
            assert_eq!(message, "A folder cannot go inside itself.");
        }
        assert_eq!(outline(&store), ["Outer/", "  Inner/"]);
    }

    #[test]
    fn nothing_can_be_put_inside_a_bookmark() {
        let mut store = PlacesStore::open_in_memory().unwrap();
        let first = bookmark(&mut store, "first", 1);
        let second = bookmark(&mut store, "second", 2);

        let message = refused(
            &mut store,
            BookmarkEdit::Move {
                id: first,
                parent: Some(second),
                index: 0,
            },
        );

        assert_eq!(message, "A bookmark cannot contain other items.");
    }

    #[test]
    fn items_that_no_longer_exist_are_reported() {
        let mut store = PlacesStore::open_in_memory().unwrap();

        for change in [
            BookmarkEdit::Delete { id: 42 },
            BookmarkEdit::Move {
                id: 42,
                parent: None,
                index: 0,
            },
            BookmarkEdit::Update {
                id: 42,
                title: "Name".to_owned(),
                url: None,
            },
            BookmarkEdit::CreateFolder {
                parent: Some(42),
                title: "Name".to_owned(),
            },
        ] {
            assert_eq!(refused(&mut store, change), "This item no longer exists.");
        }
    }

    #[test]
    fn a_folder_needs_a_name() {
        let mut store = PlacesStore::open_in_memory().unwrap();
        let id = folder(&mut store, None, "Folder");

        let created = refused(
            &mut store,
            BookmarkEdit::CreateFolder {
                parent: None,
                title: "  ".to_owned(),
            },
        );
        let renamed = refused(
            &mut store,
            BookmarkEdit::Update {
                id,
                title: String::new(),
                url: None,
            },
        );

        assert_eq!(created, "A folder needs a name.");
        assert_eq!(renamed, "A folder needs a name.");
        assert_eq!(outline(&store), ["Folder/"]);
    }

    #[test]
    fn a_bookmark_can_be_renamed_or_left_without_title() {
        let mut store = PlacesStore::open_in_memory().unwrap();
        let id = bookmark(&mut store, "page", 1);

        edit(
            &mut store,
            BookmarkEdit::Update {
                id,
                title: "Renamed".to_owned(),
                url: None,
            },
        );
        assert_eq!(outline(&store), ["Renamed"]);

        edit(
            &mut store,
            BookmarkEdit::Update {
                id,
                title: " ".to_owned(),
                url: None,
            },
        );
        assert_eq!(outline(&store), ["https://page.example/"]);
    }

    #[test]
    fn changing_an_address_replaces_the_place_and_forgets_the_icon() {
        let mut store = PlacesStore::open_in_memory().unwrap();
        let id = bookmark(&mut store, "page", 1);
        store
            .connection
            .execute("UPDATE bookmarks SET icon = x'01'", [])
            .unwrap();

        edit(
            &mut store,
            BookmarkEdit::Update {
                id,
                title: "page".to_owned(),
                url: Some(url("https://new.example/")),
            },
        );

        assert_eq!(places(&store.connection), ["https://new.example/"]);
        assert_eq!(store.bookmark_tree().unwrap()[0].icon, None);
    }

    #[test]
    fn changing_an_address_keeps_the_old_place_when_it_has_visits() {
        let mut store = PlacesStore::open_in_memory().unwrap();
        visit(&mut store, &url("https://page.example/"), date(1));
        let id = bookmark(&mut store, "page", 2);

        edit(
            &mut store,
            BookmarkEdit::Update {
                id,
                title: "page".to_owned(),
                url: Some(url("https://new.example/")),
            },
        );

        assert_eq!(
            places(&store.connection),
            ["https://page.example/", "https://new.example/"]
        );
    }

    #[test]
    fn an_address_that_is_already_bookmarked_is_refused() {
        let mut store = PlacesStore::open_in_memory().unwrap();
        let id = bookmark(&mut store, "first", 1);
        bookmark(&mut store, "second", 2);

        let message = refused(
            &mut store,
            BookmarkEdit::Update {
                id,
                title: "Changed".to_owned(),
                url: Some(url("https://second.example/")),
            },
        );

        assert_eq!(message, "This address is already bookmarked.");
        assert_eq!(outline(&store), ["first", "second"]);
    }

    #[test]
    fn deleting_a_folder_deletes_its_content_and_unused_places() {
        let mut store = PlacesStore::open_in_memory().unwrap();
        visit(&mut store, &url("https://visited.example/"), date(1));
        let visited = bookmark(&mut store, "visited", 2);
        let unvisited = bookmark(&mut store, "unvisited", 3);
        bookmark(&mut store, "kept", 4);
        let outer = folder(&mut store, None, "Outer");
        let inner = folder(&mut store, Some(outer), "Inner");
        move_to(&mut store, visited, Some(outer), 0);
        move_to(&mut store, unvisited, Some(inner), 0);

        edit(&mut store, BookmarkEdit::Delete { id: outer });

        assert_eq!(outline(&store), ["kept"]);
        assert_eq!(
            places(&store.connection),
            ["https://visited.example/", "https://kept.example/"]
        );
    }

    #[test]
    fn deleted_folders_and_bookmarks_are_erased_from_the_files() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = PlacesStore::open(&directory.path().join("places.sqlite")).unwrap();
        let id = folder(&mut store, None, "Very secret folder");
        let page = url("https://example.com/test");
        add(&mut store, &page, Some("Very secret title"), date(1));
        let inside = id_of(&store, "Very secret title");
        move_to(&mut store, inside, Some(id), 0);
        assert!(files_contain(directory.path(), "Very secret folder"));

        edit(&mut store, BookmarkEdit::Delete { id });

        assert!(!files_contain(directory.path(), "Very secret folder"));
        assert!(!files_contain(directory.path(), "Very secret title"));
        assert!(!files_contain(directory.path(), "example.com/test"));
    }

    #[test]
    fn a_bookmark_inside_a_folder_can_be_removed_by_its_address() {
        let mut store = PlacesStore::open_in_memory().unwrap();
        let page = bookmark(&mut store, "page", 1);
        let id = folder(&mut store, None, "Folder");
        move_to(&mut store, page, Some(id), 0);

        remove(&mut store, &url("https://page.example/"));

        assert_eq!(outline(&store), ["Folder/"]);
    }

    #[test]
    fn adding_a_page_already_in_a_folder_leaves_it_there() {
        let mut store = PlacesStore::open_in_memory().unwrap();
        let page = bookmark(&mut store, "page", 1);
        let id = folder(&mut store, None, "Folder");
        move_to(&mut store, page, Some(id), 0);

        add(
            &mut store,
            &url("https://page.example/"),
            Some("Again"),
            date(2),
        );

        assert_eq!(outline(&store), ["Folder/", "  page"]);
    }

    #[test]
    fn a_bookmark_keeps_the_last_icon_of_its_page() {
        let mut store = PlacesStore::open_in_memory().unwrap();
        let page = url("https://page.example/");
        add(&mut store, &page, None, date(1));

        for icon in [vec![1], vec![2, 3]] {
            store
                .apply(&[PlacesCommand::SetBookmarkIcon {
                    url: page.clone(),
                    icon,
                }])
                .unwrap();
        }

        assert_eq!(store.bookmark_tree().unwrap()[0].icon, Some(vec![2, 3]));
    }

    #[test]
    fn only_a_new_icon_changes_the_bookmarks() {
        let mut store = PlacesStore::open_in_memory().unwrap();
        let page = url("https://page.example/");
        add(&mut store, &page, None, date(1));
        let mut set_icon = |icon: Vec<u8>| {
            store
                .apply(&[PlacesCommand::SetBookmarkIcon {
                    url: page.clone(),
                    icon,
                }])
                .unwrap()
        };

        assert!(set_icon(vec![1]));
        assert!(!set_icon(vec![1]));
        assert!(set_icon(vec![2]));
    }

    #[test]
    fn the_icon_of_a_page_that_is_not_bookmarked_is_not_stored() {
        let mut store = PlacesStore::open_in_memory().unwrap();
        let page = url("https://page.example/");
        visit(&mut store, &page, date(1));

        store
            .apply(&[PlacesCommand::SetBookmarkIcon {
                url: page,
                icon: vec![1],
            }])
            .unwrap();

        assert!(store.bookmark_tree().unwrap().is_empty());
    }

    #[test]
    fn a_bookmark_with_a_visit_expired_stays_in_its_folder() {
        let mut store = PlacesStore::open_in_memory().unwrap();
        visit(&mut store, &url("https://page.example/"), date(1));
        let page = bookmark(&mut store, "page", 2);
        let id = folder(&mut store, None, "Folder");
        move_to(&mut store, page, Some(id), 0);

        store.delete_visits_before(date(50)).unwrap();

        assert_eq!(outline(&store), ["Folder/", "  page"]);
    }
}
