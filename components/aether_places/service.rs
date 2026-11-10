/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::future::Future;
use std::iter;
use std::path::PathBuf;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime};

use crossbeam_channel::{Receiver, Sender, select, tick, unbounded};
use log::{error, warn};
use tokio::sync::oneshot;
use url::Url;

use crate::PlacesStore;
use crate::database::PlacesCommand;

const EXPIRATION_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// Writes the places database from a dedicated thread, so that SQLite never blocks the caller.
///
/// Commands are applied in the order they are sent. Dropping the service waits until the
/// pending ones are written.
pub struct PlacesService {
    /// Only `None` while the service is dropped, to close the channel before joining.
    sender: Option<Sender<PlacesRequest>>,
    cleaner: HistoryCleaner,
    thread: Option<JoinHandle<()>>,
    bookmarks: Receiver<SavedBookmarks>,
}

const HISTORY_UNAVAILABLE: &str = "The history is not available";

type Reply = oneshot::Sender<Result<(), String>>;

enum CleanerRequest {
    SetRetention(Duration, Reply),
    DeleteVisitsSince(SystemTime, Reply),
}

/// Sends the saved bookmarks to the owner of a [`PlacesService`], and tells it to take them.
struct BookmarksPublisher {
    sender: Sender<SavedBookmarks>,
    notify: Box<dyn Fn() + Send>,
}

impl BookmarksPublisher {
    fn publish(&self, urls: Vec<Url>, revision: u64) {
        let _ = self.sender.send(SavedBookmarks { urls, revision });
        (self.notify)();
    }
}

/// The bookmarks stored in the database, and the newest bookmark change included in that read.
///
/// `revision` is zero when no bookmark change has been attempted yet.
pub struct SavedBookmarks {
    pub urls: Vec<Url>,
    pub revision: u64,
}

struct PlacesRequest {
    command: PlacesCommand,
    /// The interface's revision of `command` when it changes bookmarks.
    bookmark_revision: Option<u64>,
}

/// Deletes visits of a [`PlacesService`], after the changes already sent to it.
#[derive(Clone)]
pub struct HistoryCleaner(Sender<CleanerRequest>);

impl HistoryCleaner {
    /// Keeps visits for `retention`, and deletes the older ones now.
    pub fn set_retention(&self, retention: Duration) -> CleanerReply {
        self.request(|reply| CleanerRequest::SetRetention(retention, reply))
    }

    /// Deletes the visits at or after `date`.
    pub fn delete_visits_since(&self, date: SystemTime) -> CleanerReply {
        self.request(|reply| CleanerRequest::DeleteVisitsSince(date, reply))
    }

    fn request(&self, request: impl FnOnce(Reply) -> CleanerRequest) -> CleanerReply {
        let (reply, receiver) = oneshot::channel();
        let _ = self.0.send(request(reply));
        CleanerReply(receiver)
    }
}

/// Resolves when the deletion asked to a [`HistoryCleaner`] is done, with its error if any.
#[must_use]
pub struct CleanerReply(oneshot::Receiver<Result<(), String>>);

impl Future for CleanerReply {
    type Output = Result<(), String>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.0)
            .poll(context)
            .map(|result| result.unwrap_or_else(|_| Err(HISTORY_UNAVAILABLE.to_owned())))
    }
}

impl PlacesService {
    /// Starts a service writing to the places database at `path`. `notify` is called from
    /// another thread when [`Self::take_bookmarks`] has something new.
    pub fn open(path: PathBuf, retention: Duration, notify: impl Fn() + Send + 'static) -> Self {
        Self::start(move || PlacesStore::open(&path), retention, notify)
    }

    /// Starts a service whose database is never written to disk.
    pub fn open_in_memory(retention: Duration, notify: impl Fn() + Send + 'static) -> Self {
        Self::start(PlacesStore::open_in_memory, retention, notify)
    }

    fn start(
        open_store: impl FnOnce() -> rusqlite::Result<PlacesStore> + Send + 'static,
        retention: Duration,
        notify: impl Fn() + Send + 'static,
    ) -> Self {
        let (sender, receiver) = unbounded();
        let (cleaner_sender, cleaner_receiver) = unbounded();
        let (bookmarks_sender, bookmarks) = unbounded();
        let publisher = BookmarksPublisher {
            sender: bookmarks_sender,
            notify: Box::new(notify),
        };
        let thread = thread::Builder::new()
            .name("Places".to_owned())
            .spawn(move || match open_store() {
                Ok(store) => {
                    if let Some(urls) = read_bookmarks(&store) {
                        publisher.publish(urls, 0);
                    }
                    run(store, receiver, retention, cleaner_receiver, publisher)
                },
                // Nothing is saved for this session, but browsing keeps working.
                Err(error) => error!("Could not open the places database: {error}"),
            })
            .expect("Thread spawning failed");
        Self {
            sender: Some(sender),
            cleaner: HistoryCleaner(cleaner_sender),
            thread: Some(thread),
            bookmarks,
        }
    }

    /// Returns a handle to delete visits.
    pub fn cleaner(&self) -> HistoryCleaner {
        self.cleaner.clone()
    }

    /// Records a visit of `url` at `visit_date`.
    pub fn record_visit(&self, url: Url, visit_date: SystemTime, title: Option<String>) {
        self.send(PlacesCommand::RecordVisit {
            url,
            visit_date,
            title,
        });
    }

    /// Stores `title` as the last title of `url`.
    pub fn set_title(&self, url: Url, title: String) {
        self.send(PlacesCommand::SetTitle { url, title });
    }

    /// Moves the visit of `old_url` at `visit_date` to `new_url`, or deletes it when `new_url`
    /// is `None`.
    pub fn replace_visit(
        &self,
        old_url: Url,
        visit_date: SystemTime,
        new_url: Option<Url>,
        title: Option<String>,
    ) {
        self.send(PlacesCommand::ReplaceVisit {
            old_url,
            visit_date,
            new_url,
            title,
        });
    }

    /// The saved bookmarks, if they were sent since the last call: at startup and after a
    /// failed write.
    pub fn take_bookmarks(&self) -> Option<SavedBookmarks> {
        self.bookmarks.try_iter().last()
    }

    /// Bookmarks `url`, unless it already is.
    ///
    /// `revision` identifies this change. A later read reports the newest revision it includes.
    pub fn add_bookmark(
        &self,
        url: Url,
        title: Option<String>,
        date_added: SystemTime,
        revision: u64,
    ) {
        self.send_request(PlacesRequest {
            command: PlacesCommand::AddBookmark {
                url,
                title,
                date_added,
            },
            bookmark_revision: Some(revision),
        });
    }

    /// Removes the bookmark of `url`.
    ///
    /// `revision` identifies this change. A later read reports the newest revision it includes.
    pub fn remove_bookmark(&self, url: Url, revision: u64) {
        self.send_request(PlacesRequest {
            command: PlacesCommand::RemoveBookmark { url },
            bookmark_revision: Some(revision),
        });
    }

    fn send(&self, command: PlacesCommand) {
        self.send_request(PlacesRequest {
            command,
            bookmark_revision: None,
        });
    }

    fn send_request(&self, request: PlacesRequest) {
        if let Some(sender) = &self.sender {
            let _ = sender.send(request);
        }
    }
}

impl Drop for PlacesService {
    fn drop(&mut self) {
        // Closing the channel ends the thread once it has applied the pending commands.
        self.sender.take();
        if let Some(thread) = self.thread.take() &&
            thread.join().is_err()
        {
            error!("The places thread panicked");
        }
    }
}

fn read_bookmarks(store: &PlacesStore) -> Option<Vec<Url>> {
    store
        .bookmarked_urls()
        .inspect_err(|error| warn!("Could not read the bookmarks: {error}"))
        .ok()
}

fn run(
    mut store: PlacesStore,
    receiver: Receiver<PlacesRequest>,
    mut retention: Duration,
    cleaner_receiver: Receiver<CleanerRequest>,
    publisher: BookmarksPublisher,
) {
    let expiration_timer = tick(EXPIRATION_INTERVAL);
    expire_or_warn(&mut store, retention);
    loop {
        select! {
            recv(receiver) -> request => {
                let Ok(request) = request else {
                    for cleaner_request in cleaner_receiver.try_iter() {
                        clean(&mut store, cleaner_request, &mut retention);
                    }
                    return;
                };
                let requests = iter::once(request).chain(receiver.try_iter()).collect();
                write(&mut store, requests, &publisher);
            },
            recv(cleaner_receiver) -> request => {
                let Ok(request) = request else {
                    return;
                };
                write(&mut store, receiver.try_iter().collect(), &publisher);
                clean(&mut store, request, &mut retention);
            },
            recv(expiration_timer) -> _ => expire_or_warn(&mut store, retention),
        }
    }
}

fn clean(store: &mut PlacesStore, request: CleanerRequest, retention: &mut Duration) {
    let (result, reply) = match request {
        CleanerRequest::SetRetention(new_retention, reply) => {
            *retention = new_retention;
            (expire(store, *retention), reply)
        },
        CleanerRequest::DeleteVisitsSince(date, reply) => (store.delete_visits_since(date), reply),
    };
    let result = result.map_err(|error| {
        warn!("Could not delete the history: {error}");
        format!("Could not delete the history: {error}")
    });
    let _ = reply.send(result);
}

fn write(store: &mut PlacesStore, requests: Vec<PlacesRequest>, publisher: &BookmarksPublisher) {
    if requests.is_empty() {
        return;
    }
    let revision = requests
        .iter()
        .filter_map(|request| request.bookmark_revision)
        .max();
    let commands: Vec<_> = requests
        .into_iter()
        .map(|request| request.command)
        .collect();
    if let Err(error) = store.apply(&commands) {
        warn!(
            "Could not write {} changes to the places database: {error}",
            commands.len()
        );
        // The revision is the newest bookmark change of this attempt. The read is the database
        // without that attempt, so the interface can keep the changes it sent afterwards.
        if let Some(revision) = revision &&
            let Some(urls) = read_bookmarks(store)
        {
            publisher.publish(urls, revision);
        }
    }
}

fn expire(store: &mut PlacesStore, retention: Duration) -> rusqlite::Result<()> {
    match SystemTime::now().checked_sub(retention) {
        Some(date) => store.delete_visits_before(date),
        None => Ok(()),
    }
}

fn expire_or_warn(store: &mut PlacesStore, retention: Duration) {
    if let Err(error) = expire(store, retention) {
        warn!("Could not delete the expired history: {error}");
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use crossbeam_channel::{Receiver, unbounded};
    use futures::executor::block_on;
    use rusqlite::Connection;
    use url::Url;

    use super::{HISTORY_UNAVAILABLE, PlacesService, SavedBookmarks};

    fn days(days: u64) -> Duration {
        Duration::from_secs(days * 24 * 60 * 60)
    }

    fn urls(path: &Path) -> Vec<String> {
        Connection::open(path)
            .unwrap()
            .prepare("SELECT url FROM visits JOIN places ON places.id = visits.place_id")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    fn write_old_and_recent_visits(path: &Path) {
        let now = SystemTime::now();
        let service = PlacesService::open(path.to_owned(), Duration::MAX, || {});
        let old = Url::parse("https://example.com/old").unwrap();
        let recent = Url::parse("https://example.com/recent").unwrap();
        service.record_visit(old, now - days(100), None);
        service.record_visit(recent, now - days(1), None);
        drop(service);
    }

    #[test]
    fn dropping_the_service_writes_pending_visits() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("places.sqlite");
        let service = PlacesService::open(path.clone(), Duration::MAX, || {});

        for index in 0..100 {
            let url = Url::parse(&format!("https://servo.org/{index}")).unwrap();
            service.record_visit(url, UNIX_EPOCH, None);
        }
        drop(service);

        let visits: u32 = Connection::open(&path)
            .unwrap()
            .query_row("SELECT count(*) FROM visits", [], |row| row.get(0))
            .unwrap();
        assert_eq!(visits, 100);
    }

    #[test]
    fn titles_are_written_by_the_service() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("places.sqlite");
        let service = PlacesService::open(path.clone(), Duration::MAX, || {});
        let url = Url::parse("https://servo.org/").unwrap();

        service.record_visit(url.clone(), UNIX_EPOCH, None);
        service.set_title(url, "Servo".to_owned());
        drop(service);

        let title: String = Connection::open(&path)
            .unwrap()
            .query_row("SELECT title FROM places", [], |row| row.get(0))
            .unwrap();
        assert_eq!(title, "Servo");
    }

    #[test]
    fn the_last_of_many_titles_is_written() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("places.sqlite");
        let service = PlacesService::open(path.clone(), Duration::MAX, || {});
        let url = Url::parse("https://servo.org/").unwrap();

        service.record_visit(url.clone(), UNIX_EPOCH, None);
        for index in 0..10_000 {
            service.set_title(url.clone(), format!("Title {index}"));
        }
        drop(service);

        let title: String = Connection::open(&path)
            .unwrap()
            .query_row("SELECT title FROM places", [], |row| row.get(0))
            .unwrap();
        assert_eq!(title, "Title 9999");
    }

    #[test]
    fn replaced_visits_are_written_by_the_service() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("places.sqlite");
        let service = PlacesService::open(path.clone(), Duration::MAX, || {});
        let redirect = Url::parse("https://servo.org/redirect").unwrap();
        let target = Url::parse("https://servo.org/").unwrap();

        service.record_visit(redirect.clone(), UNIX_EPOCH, None);
        service.replace_visit(redirect, UNIX_EPOCH, Some(target), None);
        drop(service);

        let url: String = Connection::open(&path)
            .unwrap()
            .query_row(
                "SELECT url FROM visits JOIN places ON places.id = visits.place_id",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(url, "https://servo.org/");
    }

    #[test]
    fn a_database_that_cannot_be_opened_disables_the_history() {
        // SQLite cannot open a directory as a database.
        let directory = tempfile::tempdir().unwrap();
        let service = PlacesService::open(directory.path().to_owned(), Duration::MAX, || {});

        service.record_visit(Url::parse("https://servo.org/").unwrap(), UNIX_EPOCH, None);
        drop(service);
    }

    fn open_notified(path: PathBuf, retention: Duration) -> (PlacesService, Receiver<()>) {
        let (sender, notifications) = unbounded();
        let service = PlacesService::open(path, retention, move || {
            let _ = sender.send(());
        });
        (service, notifications)
    }

    fn next_bookmarks(service: &PlacesService, notifications: &Receiver<()>) -> SavedBookmarks {
        notifications.recv_timeout(Duration::from_secs(10)).unwrap();
        service.take_bookmarks().unwrap()
    }

    /// Waits until the commands already sent to `service` are written.
    fn flush(service: &PlacesService) {
        block_on(service.cleaner().set_retention(Duration::MAX)).unwrap();
    }

    fn reject_inserts_into(path: &Path, table: &str) {
        Connection::open(path)
            .unwrap()
            .execute_batch(&format!(
                "CREATE TRIGGER reject BEFORE INSERT ON {table}
                 BEGIN SELECT RAISE(ABORT, 'rejected'); END;"
            ))
            .unwrap();
    }

    #[test]
    fn bookmarks_are_written_by_the_service_and_loaded_at_the_next_start() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("places.sqlite");
        let page = Url::parse("https://servo.org/").unwrap();
        let (service, notifications) = open_notified(path.clone(), Duration::MAX);
        let saved = next_bookmarks(&service, &notifications);
        assert!(saved.urls.is_empty());
        assert_eq!(saved.revision, 0);

        service.add_bookmark(page.clone(), Some("Servo".to_owned()), UNIX_EPOCH, 1);
        drop(service);

        let (service, notifications) = open_notified(path, Duration::MAX);
        assert_eq!(next_bookmarks(&service, &notifications).urls, [page]);
    }

    #[test]
    fn a_removed_bookmark_is_not_loaded_at_the_next_start() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("places.sqlite");
        let page = Url::parse("https://servo.org/").unwrap();
        let service = PlacesService::open(path.clone(), Duration::MAX, || {});

        service.add_bookmark(page.clone(), None, UNIX_EPOCH, 1);
        service.remove_bookmark(page, 2);
        drop(service);

        let (service, notifications) = open_notified(path, Duration::MAX);
        assert!(next_bookmarks(&service, &notifications).urls.is_empty());
    }

    #[test]
    fn bookmarks_outlive_the_expiration_of_their_visits() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("places.sqlite");
        let page = Url::parse("https://servo.org/").unwrap();
        let service = PlacesService::open(path.clone(), Duration::MAX, || {});
        service.record_visit(page.clone(), SystemTime::now() - days(100), None);
        service.add_bookmark(page.clone(), None, SystemTime::now() - days(100), 1);
        drop(service);

        drop(PlacesService::open(path.clone(), days(90), || {}));

        assert!(urls(&path).is_empty());
        let (service, notifications) = open_notified(path, days(90));
        assert_eq!(next_bookmarks(&service, &notifications).urls, [page]);
    }

    #[test]
    fn a_database_that_cannot_be_opened_sends_no_bookmarks() {
        let directory = tempfile::tempdir().unwrap();

        let (service, notifications) = open_notified(directory.path().to_owned(), Duration::MAX);
        drop(service);

        assert!(notifications.try_recv().is_err());
    }

    #[test]
    fn bookmarks_that_cannot_be_read_at_startup_are_not_sent() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("places.sqlite");
        drop(PlacesService::open(path.clone(), Duration::MAX, || {}));
        Connection::open(&path)
            .unwrap()
            .execute_batch("ALTER TABLE bookmarks RENAME TO unreadable")
            .unwrap();

        let (service, notifications) = open_notified(path, Duration::MAX);
        drop(service);

        assert!(notifications.try_recv().is_err());
    }

    #[test]
    fn a_failed_bookmark_write_sends_the_saved_bookmarks_again() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("places.sqlite");
        let saved = Url::parse("https://servo.org/").unwrap();
        let (service, notifications) = open_notified(path.clone(), Duration::MAX);
        next_bookmarks(&service, &notifications);
        service.add_bookmark(saved.clone(), None, UNIX_EPOCH, 1);
        flush(&service);
        reject_inserts_into(&path, "bookmarks");

        service.add_bookmark(
            Url::parse("https://example.com/").unwrap(),
            None,
            UNIX_EPOCH,
            4,
        );

        let read = next_bookmarks(&service, &notifications);
        assert_eq!(read.urls, [saved]);
        assert_eq!(read.revision, 4);
    }

    #[test]
    fn only_failed_bookmark_writes_send_the_bookmarks_again() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("places.sqlite");
        let page = Url::parse("https://servo.org/").unwrap();
        let (service, notifications) = open_notified(path.clone(), Duration::MAX);
        next_bookmarks(&service, &notifications);
        reject_inserts_into(&path, "visits");

        service.record_visit(page.clone(), SystemTime::now(), None);
        flush(&service);
        service.add_bookmark(page, None, UNIX_EPOCH, 1);
        drop(service);

        assert!(notifications.try_recv().is_err());
    }

    #[test]
    fn old_visits_are_deleted_when_the_service_starts() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("places.sqlite");
        write_old_and_recent_visits(&path);

        drop(PlacesService::open(path.clone(), days(90), || {}));

        assert_eq!(urls(&path), ["https://example.com/recent"]);
    }

    #[test]
    fn a_shorter_retention_deletes_old_visits() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("places.sqlite");
        write_old_and_recent_visits(&path);
        let service = PlacesService::open(path.clone(), days(365), || {});

        assert_eq!(block_on(service.cleaner().set_retention(days(90))), Ok(()));

        assert_eq!(urls(&path), ["https://example.com/recent"]);
    }

    #[test]
    fn deleting_recent_visits_includes_the_pending_ones() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("places.sqlite");
        let now = SystemTime::now();
        let service = PlacesService::open(path.clone(), Duration::MAX, || {});
        service.record_visit(
            Url::parse("https://example.com/old").unwrap(),
            now - days(2),
            None,
        );
        for index in 0..10_000 {
            let url = Url::parse(&format!("https://example.com/{index}")).unwrap();
            service.record_visit(url, now, None);
        }

        let deletion = service.cleaner().delete_visits_since(now - days(1));
        drop(service);

        assert_eq!(block_on(deletion), Ok(()));
        assert_eq!(urls(&path), ["https://example.com/old"]);
    }

    #[test]
    fn deletion_errors_are_reported() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("places.sqlite");
        write_old_and_recent_visits(&path);
        let service = PlacesService::open(path.clone(), Duration::MAX, || {});
        assert_eq!(
            block_on(service.cleaner().set_retention(Duration::MAX)),
            Ok(())
        );
        Connection::open(&path)
            .unwrap()
            .execute_batch("DROP TABLE visits")
            .unwrap();

        assert!(block_on(service.cleaner().set_retention(days(90))).is_err());
        assert!(block_on(service.cleaner().delete_visits_since(UNIX_EPOCH)).is_err());
    }

    #[test]
    fn the_cleaner_can_outlive_the_service() {
        let service = PlacesService::open_in_memory(days(90), || {});
        let cleaner = service.cleaner();

        drop(service);

        let unavailable = Err(HISTORY_UNAVAILABLE.to_owned());
        assert_eq!(block_on(cleaner.set_retention(days(1))), unavailable);
        assert_eq!(
            block_on(cleaner.delete_visits_since(UNIX_EPOCH)),
            unavailable
        );
    }
}
