/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Profile files for the network filter edited from `about:settings#network`.
//!
//! The network stack reads the same files (`blocklist.txt` and
//! `content_filter.json`) and applies them on the next request.
//!
//! The JSON keys are the Firefox preference names, as in `home.json`:
//! - `privacy.trackingprotection.enabled`
//! - `urlclassifier.trackingWhitelistTable` — hosts that are never blocked
//! - `urlclassifier.trackingTable` — extra filter files, beside `blocklist.txt`
//!
//! Predefined lists live in another repository. Copy them by hand into
//! `{profile}/filter-lists/`. A checked list is read from that folder.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::{Value, json};

const BLOCKLIST_FILE: &str = "blocklist.txt";
const CONFIG_FILE: &str = "content_filter.json";
/// Folder inside the profile. List files are copied here by hand from the lists repository.
const FILTER_LISTS_DIR: &str = "filter-lists";
/// `(file name, label)`. The file name is what the lists repository publishes.
const PREDEFINED_LISTS: &[(&str, &str)] = &[
    ("easylist.txt", "EasyList"),
    ("easyprivacy.txt", "EasyPrivacy"),
];

const PREF_ENABLED: &str = "privacy.trackingprotection.enabled";
const PREF_WHITELIST: &str = "urlclassifier.trackingWhitelistTable";
const PREF_FILTER_LISTS: &str = "urlclassifier.trackingTable";

struct InstalledFilter {
    config_dir: Option<PathBuf>,
}

static INSTALLED: Mutex<InstalledFilter> = Mutex::new(InstalledFilter { config_dir: None });

/// Remember the profile directory so `about:content-filter` writes the list
/// the network stack is watching.
pub(crate) fn install_content_filter(config_dir: Option<PathBuf>) {
    INSTALLED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .config_dir = config_dir;
}

/// Answer `about:content-filter`. A query string saves the enable flag and the list.
pub(crate) fn handle_content_filter_request(query: Option<&str>) -> String {
    let config_dir = INSTALLED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .config_dir
        .clone();
    let Some(config_dir) = config_dir else {
        return json!({
            "enabled": true,
            "blocklist": "",
            "lists": [],
            "error": "No profile directory is available.",
        })
        .to_string();
    };

    if let Some(query) = query.filter(|query| !query.is_empty()) {
        let params: Vec<(String, String)> = url::form_urlencoded::parse(query.as_bytes())
            .into_owned()
            .collect();
        let current = load_settings(&config_dir);
        let enabled = params
            .iter()
            .find(|(name, _)| name == "enabled")
            .map(|(_, value)| value == "true")
            .unwrap_or(current.enabled);
        let blocklist = params
            .iter()
            .find(|(name, _)| name == "blocklist")
            .map(|(_, value)| value.clone())
            .unwrap_or_else(|| current.blocklist.clone());
        let lists = params.iter().any(|(name, _)| name == "lists").then(|| {
            params
                .iter()
                .find(|(name, _)| name == "lists")
                .map(|(_, value)| selected_lists(value))
                .unwrap_or_default()
        });
        if let Err(error) = save_settings(&config_dir, enabled, &blocklist, lists.as_deref()) {
            return json_settings(&config_dir, current.enabled, &current.blocklist, Some(&error));
        }
    }

    let settings = load_settings(&config_dir);
    json_settings(&config_dir, settings.enabled, &settings.blocklist, None)
}

struct FilterSettings {
    enabled: bool,
    blocklist: String,
}

fn selected_lists(value: &str) -> Vec<String> {
    value
        .split(',')
        .filter(|name| {
            PREDEFINED_LISTS
                .iter()
                .any(|(file, _)| *file == name.trim())
        })
        .map(|name| format!("{FILTER_LISTS_DIR}/{}", name.trim()))
        .collect()
}

fn load_settings(config_dir: &Path) -> FilterSettings {
    let enabled = fs::read_to_string(config_dir.join(CONFIG_FILE))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|value| {
            value
                .get(PREF_ENABLED)
                .or_else(|| value.get("enabled"))
                .and_then(Value::as_bool)
        })
        .unwrap_or(true);
    let blocklist = fs::read_to_string(config_dir.join(BLOCKLIST_FILE)).unwrap_or_default();
    FilterSettings { enabled, blocklist }
}

fn save_settings(
    config_dir: &Path,
    enabled: bool,
    blocklist: &str,
    lists: Option<&[String]>,
) -> Result<(), String> {
    fs::create_dir_all(config_dir.join(FILTER_LISTS_DIR)).map_err(|error| error.to_string())?;
    let current = fs::read_to_string(config_dir.join(CONFIG_FILE))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .unwrap_or_else(|| json!({}));
    let config = json!({
        PREF_ENABLED: enabled,
        PREF_WHITELIST: string_list(
            current
                .get(PREF_WHITELIST)
                .or_else(|| current.get("whitelist_domains")),
        ),
        PREF_FILTER_LISTS: match lists {
            Some(lists) => merge_lists(
                &string_list(
                    current
                        .get(PREF_FILTER_LISTS)
                        .or_else(|| current.get("filter_lists")),
                ),
                lists,
            ),
            None => string_list(
                current
                    .get(PREF_FILTER_LISTS)
                    .or_else(|| current.get("filter_lists")),
            ),
        },
    });
    let config_text = serde_json::to_string_pretty(&config).map_err(|error| error.to_string())?;
    let config_bytes = format!("{config_text}\n");
    let mut list = blocklist.replace("\r\n", "\n");
    if !list.is_empty() && !list.ends_with('\n') {
        list.push('\n');
    }
    let list_path = config_dir.join(BLOCKLIST_FILE);
    let config_path = config_dir.join(CONFIG_FILE);
    // Enabling must not become visible before the list it belongs with. A
    // failed second write then leaves the filter off, or already off, instead
    // of matching the previous list under the new flag.
    if enabled {
        write_atomic(&list_path, list.as_bytes())?;
        write_atomic(&config_path, config_bytes.as_bytes())
    } else {
        write_atomic(&config_path, config_bytes.as_bytes())?;
        write_atomic(&list_path, list.as_bytes())
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, bytes).map_err(|error| error.to_string())?;
    fs::rename(&temporary, path).map_err(|error| error.to_string())
}

fn string_list(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Keep files that are not one of the predefined lists, then append the checked ones.
fn merge_lists(stored: &[String], selected: &[String]) -> Vec<String> {
    let mut lists: Vec<String> = stored
        .iter()
        .filter(|path| !is_predefined(path))
        .cloned()
        .collect();
    for path in selected {
        if !lists.iter().any(|existing| existing == path) {
            lists.push(path.clone());
        }
    }
    lists
}

fn is_predefined(path: &str) -> bool {
    PREDEFINED_LISTS.iter().any(|(file, _)| {
        path == *file || path == format!("{FILTER_LISTS_DIR}/{file}")
    })
}

fn list_entries(config_dir: &Path) -> Value {
    let enabled = fs::read_to_string(config_dir.join(CONFIG_FILE))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .map(|value| {
            string_list(
                value
                    .get(PREF_FILTER_LISTS)
                    .or_else(|| value.get("filter_lists")),
            )
        })
        .unwrap_or_default();
    Value::Array(
        PREDEFINED_LISTS
            .iter()
            .map(|(file, label)| {
                let path = format!("{FILTER_LISTS_DIR}/{file}");
                json!({
                    "id": file,
                    "name": label,
                    "enabled": enabled.iter().any(|item| item == file || item == &path),
                    "present": config_dir.join(FILTER_LISTS_DIR).join(file).is_file(),
                })
            })
            .collect(),
    )
}

fn json_settings(config_dir: &Path, enabled: bool, blocklist: &str, error: Option<&str>) -> String {
    let mut value = json!({
        "enabled": enabled,
        "blocklist": blocklist,
        "lists": list_entries(config_dir),
    });
    if let Some(error) = error {
        value["error"] = Value::String(error.to_owned());
    }
    value.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "aether-filter-settings-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn saving_the_list_preserves_extra_filter_files() {
        let dir = temp_dir();
        fs::write(
            dir.join(CONFIG_FILE),
            "{ \"enabled\": true, \"filter_lists\": [\"easylist.txt\"], \"whitelist_domains\": [\"ok.example\"] }\n",
        )
        .unwrap();

        save_settings(
            &dir,
            true,
            "ads.example\nhttps://tracker.example/pixel\n",
            None,
        )
        .unwrap();

        let stored: Value =
            serde_json::from_str(&fs::read_to_string(dir.join(CONFIG_FILE)).unwrap()).unwrap();
        assert_eq!(stored["privacy.trackingprotection.enabled"], true);
        assert_eq!(stored["urlclassifier.trackingTable"][0], "easylist.txt");
        assert_eq!(
            stored["urlclassifier.trackingWhitelistTable"][0],
            "ok.example"
        );
        assert!(stored.get("enabled").is_none());
        assert_eq!(
            fs::read_to_string(dir.join(BLOCKLIST_FILE)).unwrap(),
            "ads.example\nhttps://tracker.example/pixel\n"
        );

        let loaded = load_settings(&dir);
        assert!(loaded.enabled);
        assert!(loaded.blocklist.contains("ads.example"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_request_updates_the_installed_profile() {
        let dir = temp_dir();
        install_content_filter(Some(dir.clone()));
        let body = handle_content_filter_request(Some("enabled=false&blocklist=blocked.example"));
        let value: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(value["enabled"], false);
        assert_eq!(value["blocklist"], "blocked.example\n");
        assert!(value.get("error").is_none());

        let again: Value = serde_json::from_str(&handle_content_filter_request(None)).unwrap();
        assert_eq!(again["enabled"], false);
        assert_eq!(again["blocklist"], "blocked.example\n");
        let _ = fs::remove_dir_all(&dir);
        install_content_filter(None);
    }

    #[test]
    fn checking_a_predefined_list_points_at_the_filter_lists_folder() {
        let dir = temp_dir();
        fs::create_dir_all(dir.join(FILTER_LISTS_DIR)).unwrap();
        fs::write(dir.join(FILTER_LISTS_DIR).join("easylist.txt"), "||ads.example^\n").unwrap();
        fs::write(
            dir.join(CONFIG_FILE),
            "{ \"urlclassifier.trackingTable\": [\"custom.txt\", \"easylist.txt\"] }\n",
        )
        .unwrap();

        save_settings(&dir, true, "", Some(&selected_lists("easylist.txt,nope.txt"))).unwrap();

        let stored: Value =
            serde_json::from_str(&fs::read_to_string(dir.join(CONFIG_FILE)).unwrap()).unwrap();
        assert_eq!(stored["urlclassifier.trackingTable"][0], "custom.txt");
        assert_eq!(
            stored["urlclassifier.trackingTable"][1],
            "filter-lists/easylist.txt"
        );
        assert_eq!(stored["urlclassifier.trackingTable"].as_array().unwrap().len(), 2);

        let body: Value = serde_json::from_str(&{
            install_content_filter(Some(dir.clone()));
            let body = handle_content_filter_request(None);
            install_content_filter(None);
            body
        })
        .unwrap();
        assert_eq!(body["lists"][0]["id"], "easylist.txt");
        assert_eq!(body["lists"][0]["enabled"], true);
        assert_eq!(body["lists"][0]["present"], true);
        assert_eq!(body["lists"][1]["enabled"], false);
        assert_eq!(body["lists"][1]["present"], false);
        let _ = fs::remove_dir_all(&dir);
    }
}
