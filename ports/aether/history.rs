/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Browsing history of the profile.

use std::time::SystemTime;

use aether_history::HistoryService;
use servo::{NavigationType, Opts};
use url::Url;

pub(crate) struct ActiveEntry {
    url: Url,
    visit_date: Option<SystemTime>,
}

/// How a committed navigation changes the history.
#[derive(Debug, PartialEq)]
pub(crate) enum HistoryUpdate {
    RecordVisit {
        url: Url,
        visit_date: SystemTime,
    },
    ReplaceVisit {
        old_url: Url,
        visit_date: SystemTime,
        new_url: Option<Url>,
    },
}

/// Decides how a navigation committed at `now` changes the history of a tab whose active entry
/// was `active_entry`, and returns its new active entry.
///
pub(crate) fn history_update(
    active_entry: Option<&ActiveEntry>,
    url: &Url,
    navigation_type: NavigationType,
    now: SystemTime,
) -> (Option<HistoryUpdate>, ActiveEntry) {
    if let Some(active_entry) = active_entry &&
        active_entry.url == *url
    {
        let unchanged = ActiveEntry {
            url: url.clone(),
            visit_date: active_entry.visit_date,
        };
        return (None, unchanged);
    }

    let recorded = should_record(url);
    let replaced_visit = active_entry.and_then(|active_entry| {
        active_entry
            .visit_date
            .map(|visit_date| (&active_entry.url, visit_date))
    });
    match (navigation_type, replaced_visit) {
        (NavigationType::Replace, Some((old_url, visit_date))) => (
            Some(HistoryUpdate::ReplaceVisit {
                old_url: old_url.clone(),
                visit_date,
                new_url: recorded.then(|| url.clone()),
            }),
            ActiveEntry {
                url: url.clone(),
                visit_date: recorded.then_some(visit_date),
            },
        ),
        (NavigationType::Push | NavigationType::Replace, _) if recorded => (
            Some(HistoryUpdate::RecordVisit {
                url: url.clone(),
                visit_date: now,
            }),
            ActiveEntry {
                url: url.clone(),
                visit_date: Some(now),
            },
        ),
        _ => (
            None,
            ActiveEntry {
                url: url.clone(),
                visit_date: None,
            },
        ),
    }
}

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
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use servo::NavigationType;
    use url::Url;

    use super::{ActiveEntry, HistoryUpdate, history_update, should_record};

    fn url(url: &str) -> Url {
        Url::parse(url).unwrap()
    }

    fn date(seconds: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(seconds)
    }

    fn visited(page: &str, seconds: u64) -> ActiveEntry {
        ActiveEntry {
            url: url(page),
            visit_date: Some(date(seconds)),
        }
    }

    #[test]
    fn new_entries_are_new_visits() {
        let (update, active_entry) = history_update(
            Some(&visited("https://servo.org/", 1)),
            &url("https://servo.org/blog"),
            NavigationType::Push,
            date(2),
        );

        assert_eq!(
            update,
            Some(HistoryUpdate::RecordVisit {
                url: url("https://servo.org/blog"),
                visit_date: date(2),
            })
        );
        assert_eq!(active_entry.visit_date, Some(date(2)));
    }

    #[test]
    fn going_back_or_forward_is_not_a_new_visit() {
        let (update, active_entry) = history_update(
            Some(&visited("https://servo.org/blog", 2)),
            &url("https://servo.org/"),
            NavigationType::Traverse,
            date(3),
        );

        assert_eq!(update, None);
        assert_eq!(active_entry.url, url("https://servo.org/"));
        assert_eq!(active_entry.visit_date, None);
    }

    #[test]
    fn a_replaced_entry_gives_its_visit_to_the_new_url() {
        let (update, active_entry) = history_update(
            Some(&visited("https://duckduckgo.com/l/?uddg=wikipedia", 1)),
            &url("https://www.wikipedia.org/"),
            NavigationType::Replace,
            date(2),
        );

        assert_eq!(
            update,
            Some(HistoryUpdate::ReplaceVisit {
                old_url: url("https://duckduckgo.com/l/?uddg=wikipedia"),
                visit_date: date(1),
                new_url: Some(url("https://www.wikipedia.org/")),
            })
        );
        assert_eq!(active_entry.url, url("https://www.wikipedia.org/"));
        assert_eq!(active_entry.visit_date, Some(date(1)));
    }

    #[test]
    fn replacing_with_a_page_that_is_not_recorded_deletes_the_visit() {
        let (update, active_entry) = history_update(
            Some(&visited("https://servo.org/", 1)),
            &url("about:blank"),
            NavigationType::Replace,
            date(2),
        );

        assert_eq!(
            update,
            Some(HistoryUpdate::ReplaceVisit {
                old_url: url("https://servo.org/"),
                visit_date: date(1),
                new_url: None,
            })
        );
        assert_eq!(active_entry.visit_date, None);
    }

    #[test]
    fn replacing_an_entry_without_visit_records_a_new_one() {
        let new_tab = ActiveEntry {
            url: url("servo:newtab"),
            visit_date: None,
        };

        let (update, _) = history_update(
            Some(&new_tab),
            &url("https://servo.org/"),
            NavigationType::Replace,
            date(2),
        );

        assert_eq!(
            update,
            Some(HistoryUpdate::RecordVisit {
                url: url("https://servo.org/"),
                visit_date: date(2),
            })
        );
    }

    #[test]
    fn staying_on_the_same_url_changes_nothing() {
        for navigation_type in [
            NavigationType::Push,
            NavigationType::Replace,
            NavigationType::Traverse,
        ] {
            let (update, active_entry) = history_update(
                Some(&visited("https://servo.org/", 1)),
                &url("https://servo.org/"),
                navigation_type,
                date(2),
            );
            assert_eq!(update, None);
            assert_eq!(active_entry.visit_date, Some(date(1)));
        }
    }

    #[test]
    fn pages_that_are_not_recorded_have_no_visit() {
        let (update, active_entry) =
            history_update(None, &url("servo:newtab"), NavigationType::Push, date(1));

        assert_eq!(update, None);
        assert_eq!(active_entry.visit_date, None);
    }

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
