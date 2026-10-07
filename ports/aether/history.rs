/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Browsing history of the profile.

use aether_history::HistoryService;
use servo::Opts;
use url::Url;

/// Opens the history of the profile directory. It is kept in memory when storage is temporary
/// or when there is no profile directory.
pub(crate) fn open_history(opts: &Opts) -> HistoryService {
    match &opts.config_dir {
        Some(config_dir) if !opts.temporary_storage => {
            HistoryService::open(config_dir.join("places.sqlite"))
        },
        _ => HistoryService::open_in_memory(),
    }
}

/// Whether visits of `url` belong in the history: web pages only, not internal pages
/// (`servo:`, `about:`), `data:` and `blob:` URLs, or local files.
pub(crate) fn should_record(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https")
}

#[cfg(test)]
mod tests {
    use url::Url;

    use super::should_record;

    #[test]
    fn only_web_pages_are_recorded() {
        for (url, recorded) in [
            ("https://servo.org/", true),
            ("http://example.com/page#section", true),
            ("servo:newtab", false),
            ("about:blank", false),
            ("data:text/html,hello", false),
            ("blob:https://servo.org/0c1d2e3f", false),
            ("file:///home/user/page.html", false),
        ] {
            assert_eq!(should_record(&Url::parse(url).unwrap()), recorded, "{url}");
        }
    }
}
