/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Bookmarks of the profile.

use std::collections::HashSet;

use aether_places::without_credentials;
use url::Url;

/// A bookmark change sent to the database and not yet covered by a saved read.
struct PendingToggle {
    revision: u64,
    url: Url,
    added: bool,
}

/// In-memory copy of the bookmarks of the places database, which the interface reads every frame.
pub(crate) struct Bookmarks {
    urls: HashSet<Url>,
    pending: Vec<PendingToggle>,
    next_revision: u64,
}

impl Bookmarks {
    pub(crate) fn new(urls: &[Url]) -> Self {
        Self {
            urls: urls.iter().cloned().collect(),
            pending: Vec::new(),
            next_revision: 1,
        }
    }

    pub(crate) fn contains(&self, url: &Url) -> bool {
        self.urls.contains(&without_credentials(url))
    }

    /// Adds `url` when it is absent and removes it otherwise.
    ///
    /// Returns whether it was added, and the revision of this change.
    #[cfg_attr(target_env = "ohos", expect(dead_code))]
    pub(crate) fn toggle(&mut self, url: &Url) -> (bool, u64) {
        let url = without_credentials(url);
        let added = !self.urls.remove(&url);
        if added {
            self.urls.insert(url.clone());
        }
        let revision = self.next_revision;
        self.next_revision += 1;
        self.pending.push(PendingToggle {
            revision,
            url,
            added,
        });
        (added, revision)
    }

    /// Replaces these bookmarks with the saved `urls`, then reapplies every local change newer
    /// than `revision`.
    #[cfg_attr(target_env = "ohos", expect(dead_code))]
    pub(crate) fn reconcile(&mut self, urls: &[Url], revision: u64) {
        self.pending.retain(|toggle| toggle.revision > revision);
        self.urls = urls.iter().cloned().collect();
        for toggle in &self.pending {
            if toggle.added {
                self.urls.insert(toggle.url.clone());
            } else {
                self.urls.remove(&toggle.url);
            }
        }
    }
}

pub(crate) fn can_bookmark(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https" | "file")
}

#[cfg(test)]
mod tests {
    use url::Url;

    use super::{Bookmarks, can_bookmark};

    fn url(url: &str) -> Url {
        Url::parse(url).unwrap()
    }

    #[test]
    fn toggling_a_page_adds_it_then_removes_it() {
        let mut bookmarks = Bookmarks::new(&[]);
        let page = url("https://servo.org/");
        assert!(!bookmarks.contains(&page));

        assert!(bookmarks.toggle(&page).0);
        assert!(bookmarks.contains(&page));

        assert!(!bookmarks.toggle(&page).0);
        assert!(!bookmarks.contains(&page));
    }

    #[test]
    fn saved_bookmarks_are_restored() {
        let page = url("https://servo.org/");

        let mut bookmarks = Bookmarks::new(std::slice::from_ref(&page));

        assert!(bookmarks.contains(&page));
        assert!(!bookmarks.toggle(&page).0);
    }

    #[test]
    fn a_bookmark_only_marks_its_exact_url() {
        let mut bookmarks = Bookmarks::new(&[]);
        bookmarks.toggle(&url("https://servo.org/blog"));

        for other in [
            "https://servo.org/",
            "https://servo.org/blog#latest",
            "https://servo.org/blog?page=2",
            "http://servo.org/blog",
        ] {
            assert!(!bookmarks.contains(&url(other)), "{other}");
        }
    }

    #[test]
    fn credentials_do_not_change_the_bookmarked_page() {
        let mut bookmarks = Bookmarks::new(&[]);

        bookmarks.toggle(&url("https://user:secret@example.com/page"));

        assert!(bookmarks.contains(&url("https://example.com/page")));
        assert!(bookmarks.contains(&url("https://other@example.com/page")));
    }

    #[test]
    fn only_web_pages_and_local_files_can_be_bookmarked() {
        for (page, allowed) in [
            ("https://servo.org/", true),
            ("http://example.com/page#section", true),
            ("file:///home/user/page.html", true),
            ("about:newtab", false),
            ("about:blank", false),
            ("about:history", false),
            ("data:text/html,hello", false),
            ("blob:https://servo.org/0c1d2e3f", false),
        ] {
            assert_eq!(can_bookmark(&url(page)), allowed, "{page}");
        }
    }

    #[test]
    fn a_saved_read_keeps_a_bookmark_changed_after_it() {
        let mut bookmarks = Bookmarks::new(&[]);
        let rolled_back = url("https://servo.org/");
        let added_later = url("https://example.com/");
        let (added, failed_revision) = bookmarks.toggle(&rolled_back);
        assert!(added);
        let (added, _) = bookmarks.toggle(&added_later);
        assert!(added);

        // The failed attempt included only the first change, so the database does not have it.
        bookmarks.reconcile(&[], failed_revision);

        assert!(!bookmarks.contains(&rolled_back));
        assert!(bookmarks.contains(&added_later));
    }

    #[test]
    fn a_failed_removal_comes_back_when_nothing_changed_afterwards() {
        let page = url("https://servo.org/");
        let mut bookmarks = Bookmarks::new(std::slice::from_ref(&page));
        let (added, revision) = bookmarks.toggle(&page);
        assert!(!added);

        bookmarks.reconcile(std::slice::from_ref(&page), revision);

        assert!(bookmarks.contains(&page));
    }
}
