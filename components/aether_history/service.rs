/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::path::PathBuf;
use std::thread::{self, JoinHandle};
use std::time::SystemTime;

use crossbeam_channel::{Receiver, Sender, unbounded};
use log::{error, warn};
use url::Url;

use crate::HistoryStore;

enum HistoryCommand {
    RecordVisit {
        url: Url,
        visit_date: SystemTime,
        title: Option<String>,
    },
    SetTitle {
        url: Url,
        title: String,
    },
    ReplaceVisit {
        old_url: Url,
        visit_date: SystemTime,
        new_url: Option<Url>,
        title: Option<String>,
    },
}

/// Writes the history from a dedicated thread, so that SQLite never blocks the caller.
///
/// Commands are applied in the order they are sent. Dropping the service waits until the
/// pending ones are written.
pub struct HistoryService {
    /// Only `None` while the service is dropped, to close the channel before joining.
    sender: Option<Sender<HistoryCommand>>,
    thread: Option<JoinHandle<()>>,
}

impl HistoryService {
    /// Starts a service writing to the history database at `path`.
    pub fn open(path: PathBuf) -> Self {
        Self::start(move || HistoryStore::open(&path))
    }

    /// Starts a service whose history is never written to disk.
    pub fn open_in_memory() -> Self {
        Self::start(HistoryStore::open_in_memory)
    }

    fn start(open_store: impl FnOnce() -> rusqlite::Result<HistoryStore> + Send + 'static) -> Self {
        let (sender, receiver) = unbounded();
        let thread = thread::Builder::new()
            .name("History".to_owned())
            .spawn(move || match open_store() {
                Ok(store) => run(store, receiver),
                // The history is disabled for this session, but browsing keeps working.
                Err(error) => error!("Could not open the history database: {error}"),
            })
            .expect("Thread spawning failed");
        Self {
            sender: Some(sender),
            thread: Some(thread),
        }
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

fn run(mut store: HistoryStore, receiver: Receiver<HistoryCommand>) {
    for command in receiver {
        match command {
            HistoryCommand::RecordVisit {
                url,
                visit_date,
                title,
            } => {
                if let Err(error) = store.record_visit(&url, visit_date, title.as_deref()) {
                    warn!("Could not record a visit in the history: {error}");
                }
            },
            HistoryCommand::SetTitle { url, title } => {
                if let Err(error) = store.set_title(&url, &title) {
                    warn!("Could not store a page title in the history: {error}");
                }
            },
            HistoryCommand::ReplaceVisit {
                old_url,
                visit_date,
                new_url,
                title,
            } => {
                if let Err(error) =
                    store.replace_visit(&old_url, visit_date, new_url.as_ref(), title.as_deref())
                {
                    warn!("Could not replace a visit in the history: {error}");
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::UNIX_EPOCH;

    use rusqlite::Connection;
    use url::Url;

    use super::HistoryService;

    #[test]
    fn dropping_the_service_writes_pending_visits() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("places.sqlite");
        let service = HistoryService::open(path.clone());

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
        let service = HistoryService::open(path.clone());
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
    fn replaced_visits_are_written_by_the_service() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("places.sqlite");
        let service = HistoryService::open(path.clone());
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
        let service = HistoryService::open(directory.path().to_owned());

        service.record_visit(Url::parse("https://servo.org/").unwrap(), UNIX_EPOCH, None);
        drop(service);
    }
}
