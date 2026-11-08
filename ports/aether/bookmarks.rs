/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Bookmarks of the profile.

use std::collections::HashSet;

use url::Url;

/// The bookmarked pages, kept in memory for the session.
#[derive(Default)]
#[cfg_attr(any(target_os = "android", target_env = "ohos"), expect(dead_code))]
pub(crate) struct Bookmarks {
    urls: HashSet<Url>,
}

#[cfg_attr(any(target_os = "android", target_env = "ohos"), expect(dead_code))]
impl Bookmarks {
    pub(crate) fn contains(&self, url: &Url) -> bool {
        self.urls.contains(&without_credentials(url))
    }

    pub(crate) fn toggle(&mut self, url: &Url) {
        let url = without_credentials(url);
        if !self.urls.remove(&url) {
            self.urls.insert(url);
        }
    }
}

#[cfg_attr(any(target_os = "android", target_env = "ohos"), expect(dead_code))]
pub(crate) fn can_bookmark(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https" | "file")
}

#[cfg_attr(any(target_os = "android", target_env = "ohos"), expect(dead_code))]
fn without_credentials(url: &Url) -> Url {
    let mut url = url.clone();
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url
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
        let mut bookmarks = Bookmarks::default();
        let page = url("https://servo.org/");
        assert!(!bookmarks.contains(&page));

        bookmarks.toggle(&page);
        assert!(bookmarks.contains(&page));

        bookmarks.toggle(&page);
        assert!(!bookmarks.contains(&page));
    }

    #[test]
    fn a_bookmark_only_marks_its_exact_url() {
        let mut bookmarks = Bookmarks::default();
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
        let mut bookmarks = Bookmarks::default();

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
}
