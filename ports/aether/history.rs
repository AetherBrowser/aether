/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Browsing history of the profile.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aether_history::{HistoryCleaner, HistoryService};
use log::warn;
use serde_json::Value;
use servo::{NavigationType, Opts};
use url::Url;

const HISTORY_SETTINGS_FILE: &str = "history.json";

const DEFAULT_RETENTION_DAYS: u32 = 90;

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

/// Opens the history of the profile directory and its settings. Both are kept in memory when
/// storage is temporary or when there is no profile directory.
pub(crate) fn open_history(opts: &Opts) -> (HistoryService, HistoryPage) {
    let config_dir = opts.config_dir.clone().filter(|_| !opts.temporary_storage);
    let retention_days = config_dir
        .as_deref()
        .map_or(DEFAULT_RETENTION_DAYS, load_retention_days);
    let retention = retention_period(retention_days);
    let history = match &config_dir {
        Some(config_dir) => HistoryService::open(config_dir.join("places.sqlite"), retention),
        None => HistoryService::open_in_memory(retention),
    };
    let page = HistoryPage {
        config_dir,
        retention_days,
        cleaner: history.cleaner(),
    };
    (history, page)
}

/// The settings and actions of the `servo:history` page.
#[cfg_attr(any(target_os = "android", target_env = "ohos"), expect(dead_code))]
pub(crate) struct HistoryPage {
    /// Where `history.json` is saved, or `None` when the settings stay in memory.
    config_dir: Option<PathBuf>,
    retention_days: u32,
    cleaner: HistoryCleaner,
}

#[cfg_attr(any(target_os = "android", target_env = "ohos"), expect(dead_code))]
impl HistoryPage {
    /// Answers `servo:history-data`, applying its `retentionDays` and `delete` parameters.
    pub(crate) fn handle_request(&mut self, query: Option<&str>) -> String {
        let mut error = None;
        for (name, value) in url::form_urlencoded::parse(query.unwrap_or_default().as_bytes()) {
            let result = match &*name {
                "retentionDays" => self.set_retention_days(&value),
                "delete" => self.delete_visits(&value),
                _ => continue,
            };
            error = error.or(result.err());
        }
        let mut response = serde_json::json!({ "retentionDays": self.retention_days });
        if let Some(error) = error {
            response["error"] = Value::String(error);
        }
        response.to_string()
    }

    fn set_retention_days(&mut self, days: &str) -> Result<(), String> {
        let Some(days) = days.parse::<u32>().ok().filter(|days| *days > 0) else {
            return Err(format!("Invalid number of days: {days}"));
        };
        if let Some(config_dir) = &self.config_dir {
            save_retention_days(config_dir, days)?;
        }
        self.retention_days = days;
        self.cleaner.set_retention(retention_period(days));
        Ok(())
    }

    fn delete_visits(&self, range: &str) -> Result<(), String> {
        let Some(start) = deletion_start(range, SystemTime::now()) else {
            return Err(format!("Invalid time range: {range}"));
        };
        self.cleaner.delete_visits_since(start);
        Ok(())
    }
}

fn retention_period(days: u32) -> Duration {
    Duration::from_secs(u64::from(days) * 24 * 60 * 60)
}

/// When a deletion of the last `range` minutes, or of everything with `all`, starts.
#[cfg_attr(any(target_os = "android", target_env = "ohos"), expect(dead_code))]
fn deletion_start(range: &str, now: SystemTime) -> Option<SystemTime> {
    if range == "all" {
        return Some(UNIX_EPOCH);
    }
    let minutes = range.parse::<u64>().ok().filter(|minutes| *minutes > 0)?;
    let range = Duration::from_secs(minutes.checked_mul(60)?);
    Some(now.checked_sub(range).unwrap_or(UNIX_EPOCH).max(UNIX_EPOCH))
}

fn load_retention_days(config_dir: &Path) -> u32 {
    let path = config_dir.join(HISTORY_SETTINGS_FILE);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == ErrorKind::NotFound => return DEFAULT_RETENTION_DAYS,
        Err(error) => {
            warn!("Could not read {}: {error}", path.display());
            return DEFAULT_RETENTION_DAYS;
        },
    };
    parse_retention_days(&text).unwrap_or_else(|| {
        warn!("Ignoring {}: no valid retentionDays", path.display());
        DEFAULT_RETENTION_DAYS
    })
}

fn parse_retention_days(json: &str) -> Option<u32> {
    let value: Value = serde_json::from_str(json).ok()?;
    let days = value.get("retentionDays")?.as_u64()?;
    u32::try_from(days).ok().filter(|days| *days > 0)
}

#[cfg_attr(any(target_os = "android", target_env = "ohos"), expect(dead_code))]
fn save_retention_days(config_dir: &Path, days: u32) -> Result<(), String> {
    let text = serde_json::json!({ "version": 1, "retentionDays": days });
    fs::create_dir_all(config_dir)
        .and_then(|()| {
            fs::write(
                config_dir.join(HISTORY_SETTINGS_FILE),
                format!("{text:#}\n"),
            )
        })
        .map_err(|error| format!("Could not save the history settings: {error}"))
}

/// Whether visits of `url` belong in the history: web pages only, not internal pages
/// (`servo:`, `about:`), `data:` and `blob:` URLs, or local files.
pub(crate) fn should_record(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https")
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use aether_history::HistoryService;
    use serde_json::{Value, json};
    use servo::NavigationType;
    use url::Url;

    use super::{
        ActiveEntry, HistoryPage, HistoryUpdate, deletion_start, history_update,
        load_retention_days, parse_retention_days, should_record,
    };

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

    fn response(page: &mut HistoryPage, query: Option<&str>) -> Value {
        serde_json::from_str(&page.handle_request(query)).unwrap()
    }

    #[test]
    fn the_retention_period_is_read_from_the_settings_file() {
        for (json, days) in [
            (r#"{ "version": 1, "retentionDays": 7 }"#, Some(7)),
            (r#"{ "retentionDays": 0 }"#, None),
            (r#"{ "retentionDays": -1 }"#, None),
            (r#"{ "retentionDays": "7" }"#, None),
            (r#"{ "retentionDays": 9999999999 }"#, None),
            ("{}", None),
            ("not json", None),
        ] {
            assert_eq!(parse_retention_days(json), days, "{json}");
        }
    }

    #[test]
    fn history_is_kept_90_days_without_settings_file() {
        let directory = tempfile::tempdir().unwrap();

        assert_eq!(load_retention_days(directory.path()), 90);
    }

    #[test]
    fn a_new_retention_period_is_saved() {
        let directory = tempfile::tempdir().unwrap();
        let history = HistoryService::open_in_memory(Duration::MAX);
        let mut page = HistoryPage {
            config_dir: Some(directory.path().to_owned()),
            retention_days: 90,
            cleaner: history.cleaner(),
        };

        assert_eq!(
            response(&mut page, Some("retentionDays=7")),
            json!({ "retentionDays": 7 })
        );
        assert_eq!(response(&mut page, None), json!({ "retentionDays": 7 }));
        assert_eq!(load_retention_days(directory.path()), 7);
    }

    #[test]
    fn an_invalid_retention_period_is_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let history = HistoryService::open_in_memory(Duration::MAX);
        let mut page = HistoryPage {
            config_dir: Some(directory.path().to_owned()),
            retention_days: 90,
            cleaner: history.cleaner(),
        };

        for days in ["0", "-7", "seven", ""] {
            let response = response(&mut page, Some(&format!("retentionDays={days}")));
            assert_eq!(response["retentionDays"], 90, "{days}");
            assert!(response["error"].is_string(), "{days}");
        }
        assert!(fs::read_dir(directory.path()).unwrap().next().is_none());
    }

    #[test]
    fn deletions_start_minutes_ago_or_at_the_epoch() {
        let now = UNIX_EPOCH + Duration::from_secs(2_000_000_000);
        for (range, start) in [
            ("60", Some(now - Duration::from_secs(3600))),
            ("40320", Some(now - Duration::from_secs(40320 * 60))),
            ("all", Some(UNIX_EPOCH)),
            ("99999999", Some(UNIX_EPOCH)),
            ("18446744073709551615", None),
            ("0", None),
            ("-60", None),
            ("hour", None),
            ("", None),
        ] {
            assert_eq!(deletion_start(range, now), start, "{range}");
        }
    }

    #[test]
    fn an_invalid_deletion_is_rejected() {
        let history = HistoryService::open_in_memory(Duration::MAX);
        let mut page = HistoryPage {
            config_dir: None,
            retention_days: 90,
            cleaner: history.cleaner(),
        };

        assert!(response(&mut page, Some("delete=hour"))["error"].is_string());
        assert_eq!(
            response(&mut page, Some("delete=60")),
            json!({ "retentionDays": 90 })
        );
    }
}
