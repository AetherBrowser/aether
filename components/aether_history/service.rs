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
    RecordVisit { url: Url, visit_date: SystemTime },
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
    pub fn record_visit(&self, url: Url, visit_date: SystemTime) {
        if let Some(sender) = &self.sender {
            let _ = sender.send(HistoryCommand::RecordVisit { url, visit_date });
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
            HistoryCommand::RecordVisit { url, visit_date } => {
                if let Err(error) = store.record_visit(&url, visit_date) {
                    warn!("Could not record a visit in the history: {error}");
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
            service.record_visit(url, UNIX_EPOCH);
        }
        drop(service);

        let visits: u32 = Connection::open(&path)
            .unwrap()
            .query_row("SELECT count(*) FROM visits", [], |row| row.get(0))
            .unwrap();
        assert_eq!(visits, 100);
    }

    #[test]
    fn a_database_that_cannot_be_opened_disables_the_history() {
        // SQLite cannot open a directory as a database.
        let directory = tempfile::tempdir().unwrap();
        let service = HistoryService::open(directory.path().to_owned());

        service.record_visit(Url::parse("https://servo.org/").unwrap(), UNIX_EPOCH);
        drop(service);
    }
}
