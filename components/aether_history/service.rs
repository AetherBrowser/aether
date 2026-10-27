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

use crate::HistoryStore;
use crate::store::HistoryCommand;

const EXPIRATION_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// Writes the history from a dedicated thread, so that SQLite never blocks the caller.
///
/// Commands are applied in the order they are sent. Dropping the service waits until the
/// pending ones are written.
pub struct HistoryService {
    /// Only `None` while the service is dropped, to close the channel before joining.
    sender: Option<Sender<HistoryCommand>>,
    cleaner: HistoryCleaner,
    thread: Option<JoinHandle<()>>,
}

const HISTORY_UNAVAILABLE: &str = "The history is not available";

type Reply = oneshot::Sender<Result<(), String>>;

enum CleanerRequest {
    SetRetention(Duration, Reply),
    DeleteVisitsSince(SystemTime, Reply),
}

/// Deletes visits of a [`HistoryService`], after the changes already sent to it.
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

impl HistoryService {
    /// Starts a service writing to the history database at `path`.
    pub fn open(path: PathBuf, retention: Duration) -> Self {
        Self::start(move || HistoryStore::open(&path), retention)
    }

    /// Starts a service whose history is never written to disk.
    pub fn open_in_memory(retention: Duration) -> Self {
        Self::start(HistoryStore::open_in_memory, retention)
    }

    fn start(
        open_store: impl FnOnce() -> rusqlite::Result<HistoryStore> + Send + 'static,
        retention: Duration,
    ) -> Self {
        let (sender, receiver) = unbounded();
        let (cleaner_sender, cleaner_receiver) = unbounded();
        let thread = thread::Builder::new()
            .name("History".to_owned())
            .spawn(move || match open_store() {
                Ok(store) => run(store, receiver, retention, cleaner_receiver),
                // The history is disabled for this session, but browsing keeps working.
                Err(error) => error!("Could not open the history database: {error}"),
            })
            .expect("Thread spawning failed");
        Self {
            sender: Some(sender),
            cleaner: HistoryCleaner(cleaner_sender),
            thread: Some(thread),
        }
    }

    /// Returns a handle to delete visits.
    pub fn cleaner(&self) -> HistoryCleaner {
        self.cleaner.clone()
    }

    /// Records a visit of `url` at `visit_date`.
    pub fn record_visit(&self, url: Url, visit_date: SystemTime, title: Option<String>) {
        if let Some(sender) = &self.sender {
            let _ = sender.send(HistoryCommand::RecordVisit {
                url,
                visit_date,
                title,
            });
        }
    }

    /// Stores `title` as the last title of `url`.
    pub fn set_title(&self, url: Url, title: String) {
        if let Some(sender) = &self.sender {
            let _ = sender.send(HistoryCommand::SetTitle { url, title });
        }
    }

    /// Moves the visit of `old_url` at `visit_date` to `new_url`, or deletes it when `new_url`
    /// is `None`. See [`HistoryStore::replace_visit`].
    pub fn replace_visit(
        &self,
        old_url: Url,
        visit_date: SystemTime,
        new_url: Option<Url>,
        title: Option<String>,
    ) {
        if let Some(sender) = &self.sender {
            let _ = sender.send(HistoryCommand::ReplaceVisit {
                old_url,
                visit_date,
                new_url,
                title,
            });
        }
    }
}

impl Drop for HistoryService {
    fn drop(&mut self) {
        // Closing the channel ends the thread once it has applied the pending commands.
        self.sender.take();
        if let Some(thread) = self.thread.take() &&
            thread.join().is_err()
        {
            error!("The history thread panicked");
        }
    }
}

fn run(
    mut store: HistoryStore,
    receiver: Receiver<HistoryCommand>,
    mut retention: Duration,
    cleaner_receiver: Receiver<CleanerRequest>,
) {
    let expiration_timer = tick(EXPIRATION_INTERVAL);
    expire_or_warn(&mut store, retention);
    loop {
        select! {
            recv(receiver) -> command => {
                let Ok(command) = command else {
                    for request in cleaner_receiver.try_iter() {
                        clean(&mut store, request, &mut retention);
                    }
                    return;
                };
                write(&mut store, iter::once(command).chain(receiver.try_iter()).collect());
            },
            recv(cleaner_receiver) -> request => {
                let Ok(request) = request else {
                    return;
                };
                write(&mut store, receiver.try_iter().collect());
                clean(&mut store, request, &mut retention);
            },
            recv(expiration_timer) -> _ => expire_or_warn(&mut store, retention),
        }
    }
}

fn clean(store: &mut HistoryStore, request: CleanerRequest, retention: &mut Duration) {
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

fn write(store: &mut HistoryStore, commands: Vec<HistoryCommand>) {
    if commands.is_empty() {
        return;
    }
    if let Err(error) = store.apply(&commands) {
        warn!(
            "Could not write {} changes to the history: {error}",
            commands.len()
        );
    }
}

fn expire(store: &mut HistoryStore, retention: Duration) -> rusqlite::Result<()> {
    match SystemTime::now().checked_sub(retention) {
        Some(date) => store.delete_visits_before(date),
        None => Ok(()),
    }
}

fn expire_or_warn(store: &mut HistoryStore, retention: Duration) {
    if let Err(error) = expire(store, retention) {
        warn!("Could not delete the expired history: {error}");
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use futures::executor::block_on;
    use rusqlite::Connection;
    use url::Url;

    use super::{HISTORY_UNAVAILABLE, HistoryService};

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
        let service = HistoryService::open(path.to_owned(), Duration::MAX);
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
        let service = HistoryService::open(path.clone(), Duration::MAX);

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
        let service = HistoryService::open(path.clone(), Duration::MAX);
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
        let service = HistoryService::open(path.clone(), Duration::MAX);
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
        let service = HistoryService::open(path.clone(), Duration::MAX);
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
        let service = HistoryService::open(directory.path().to_owned(), Duration::MAX);

        service.record_visit(Url::parse("https://servo.org/").unwrap(), UNIX_EPOCH, None);
        drop(service);
    }

    #[test]
    fn old_visits_are_deleted_when_the_service_starts() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("places.sqlite");
        write_old_and_recent_visits(&path);

        drop(HistoryService::open(path.clone(), days(90)));

        assert_eq!(urls(&path), ["https://example.com/recent"]);
    }

    #[test]
    fn a_shorter_retention_deletes_old_visits() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("places.sqlite");
        write_old_and_recent_visits(&path);
        let service = HistoryService::open(path.clone(), days(365));

        assert_eq!(block_on(service.cleaner().set_retention(days(90))), Ok(()));

        assert_eq!(urls(&path), ["https://example.com/recent"]);
    }

    #[test]
    fn deleting_recent_visits_includes_the_pending_ones() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("places.sqlite");
        let now = SystemTime::now();
        let service = HistoryService::open(path.clone(), Duration::MAX);
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
        let service = HistoryService::open(path.clone(), Duration::MAX);
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
        let service = HistoryService::open_in_memory(days(90));
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
