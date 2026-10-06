/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Search engines configured for the shell.
//!
//! Firefox stores this choice in the profile as `search.json.mozlz4`: the
//! configured engines, plus `defaultEngineId` (user choice) falling back to
//! `appDefaultEngineId`. Built-in engines are recorded by id. An engine the
//! user added also stores its URL template (`{searchTerms}`).
//!
//! Aether keeps the same split in plain JSON at `{config_dir}/search.json`.
//! `{searchTerms}` is accepted and stored as `%s`, which the location bar
//! already substitutes. Changes from `servo:settings#search` are written back
//! to that file.

use std::cmp::Ordering;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::Value;

/// User settings file, next to `prefs.json` in the config directory.
pub(crate) const SEARCH_SETTINGS_FILE: &str = "search.json";

const APP_DEFAULT_ENGINE_ID: &str = "ddg";

/// An engine shipped with Aether. The user file refers to these by [`Self::id`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BuiltinEngine {
    pub id: &'static str,
    pub name: &'static str,
    /// Search URL template. `%s` stands in for the query.
    pub url: &'static str,
}

const BUILTIN_ENGINES: &[BuiltinEngine] = &[
    BuiltinEngine {
        id: "ddg",
        name: "DuckDuckGo",
        url: "https://duckduckgo.com/html/?q=%s",
    },
    BuiltinEngine {
        id: "google",
        name: "Google",
        url: "https://www.google.com/search?q=%s",
    },
    BuiltinEngine {
        id: "bing",
        name: "Bing",
        url: "https://www.bing.com/search?q=%s",
    },
    BuiltinEngine {
        id: "startpage",
        name: "Startpage",
        url: "https://www.startpage.com/sp/search?query=%s",
    },
];

/// One configured search engine, built-in or added by the user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SearchEngine {
    pub id: String,
    pub name: String,
    /// Search URL template. `%s` stands in for the query.
    pub url: String,
    pub alias: Option<String>,
    pub order: Option<u32>,
    /// `true` when this engine comes from [`builtin_engines`] and has no custom URL.
    pub builtin: bool,
}

/// The engines configured for this profile, and which one is the default.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SearchEngines {
    engines: Vec<SearchEngine>,
    default_id: String,
}

impl SearchEngines {
    /// Every built-in engine, with DuckDuckGo as the default.
    ///
    /// Used when `search.json` is missing or cannot be read.
    pub(crate) fn builtin() -> Self {
        let engines = BUILTIN_ENGINES
            .iter()
            .map(|engine| SearchEngine {
                id: engine.id.to_owned(),
                name: engine.name.to_owned(),
                url: engine.url.to_owned(),
                alias: None,
                order: None,
                builtin: true,
            })
            .collect();
        Self {
            engines,
            default_id: APP_DEFAULT_ENGINE_ID.to_owned(),
        }
    }

    /// Engines the application knows how to search with, whether or not the
    /// user has them in `search.json`.
    #[cfg(test)]
    pub(crate) fn builtin_catalog() -> &'static [BuiltinEngine] {
        BUILTIN_ENGINES
    }

    /// Every configured engine, in display order.
    #[cfg(test)]
    pub(crate) fn engines(&self) -> &[SearchEngine] {
        &self.engines
    }

    pub(crate) fn default_engine(&self) -> &SearchEngine {
        self.engines
            .iter()
            .find(|engine| engine.id == self.default_id)
            .or_else(|| self.engines.first())
            .expect("search engines are non-empty")
    }

    /// Configured engines other than the default.
    #[cfg(test)]
    pub(crate) fn additional_engines(&self) -> impl Iterator<Item = &SearchEngine> {
        let default_id = self.default_id.clone();
        self.engines
            .iter()
            .filter(move |engine| engine.id != default_id)
    }

    pub(crate) fn set_default(&mut self, id: &str) -> Result<(), String> {
        if !self.engines.iter().any(|engine| engine.id == id) {
            return Err(format!("Unknown search engine {id}"));
        }
        self.default_id = id.to_owned();
        Ok(())
    }

    pub(crate) fn remove(&mut self, id: &str) -> Result<(), String> {
        if self.engines.len() <= 1 {
            return Err("At least one search engine is required".to_owned());
        }
        let Some(index) = self.engines.iter().position(|engine| engine.id == id) else {
            return Err(format!("Unknown search engine {id}"));
        };
        self.engines.remove(index);
        if self.default_id == id {
            self.default_id = self.engines[0].id.clone();
        }
        Ok(())
    }

    pub(crate) fn add_builtin(&mut self, id: &str) -> Result<(), String> {
        if self.engines.iter().any(|engine| engine.id == id) {
            return Err(format!("{id} is already configured"));
        }
        let Some(builtin) = builtin_by_id(id) else {
            return Err(format!("Unknown built-in search engine {id}"));
        };
        self.engines.push(SearchEngine {
            id: builtin.id.to_owned(),
            name: builtin.name.to_owned(),
            url: builtin.url.to_owned(),
            alias: None,
            order: None,
            builtin: true,
        });
        Ok(())
    }

    pub(crate) fn add_user(&mut self, name: &str, url: &str) -> Result<(), String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("Search engine name is required".to_owned());
        }
        let Some(url) = normalize_search_url(url) else {
            return Err("Search URL must contain %s or {searchTerms}".to_owned());
        };
        let id = unique_engine_id(self, &slug(name));
        self.engines.push(SearchEngine {
            id,
            name: name.to_owned(),
            url,
            alias: None,
            order: None,
            builtin: false,
        });
        Ok(())
    }
}

struct InstalledSearchEngines {
    config_dir: Option<PathBuf>,
    engines: SearchEngines,
}

static INSTALLED: Mutex<Option<InstalledSearchEngines>> = Mutex::new(None);

/// Remember the engines loaded at startup so `servo:settings#search` and the
/// location bar share one list.
pub(crate) fn install_search_engines(config_dir: Option<PathBuf>, engines: SearchEngines) {
    *INSTALLED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(InstalledSearchEngines {
        config_dir,
        engines,
    });
}

/// URL template of the default engine, once [`install_search_engines`] has run.
pub(crate) fn default_search_url() -> Option<String> {
    INSTALLED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .as_ref()
        .map(|state| state.engines.default_engine().url.clone())
}

/// Answer `servo:search-engines`. A query string applies one change and saves it.
pub(crate) fn handle_search_engines_request(query: Option<&str>) -> String {
    let mut installed = INSTALLED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(state) = installed.as_mut() else {
        return api_json(&SearchEngines::builtin(), None);
    };

    let mut error = None;
    if let Some(query) = query.filter(|query| !query.is_empty()) {
        let params: Vec<(String, String)> = url::form_urlencoded::parse(query.as_bytes())
            .into_owned()
            .collect();
        let mut updated = state.engines.clone();
        let result = apply_search_change(&mut updated, &params);
        match result {
            Ok(()) => match state
                .config_dir
                .as_deref()
                .map(|dir| save_search_engines(dir, &updated))
                .transpose()
            {
                Ok(_) => state.engines = updated,
                Err(save_error) => error = Some(save_error),
            },
            Err(change_error) => error = Some(change_error),
        }
    }

    api_json(&state.engines, error.as_deref())
}

/// Load `search.json` from `config_dir`.
///
/// A missing directory, a missing file, or a file that cannot be used falls
/// back to [`SearchEngines::builtin`].
pub(crate) fn load_search_engines(config_dir: Option<&Path>) -> SearchEngines {
    let Some(config_dir) = config_dir else {
        return SearchEngines::builtin();
    };
    let path = config_dir.join(SEARCH_SETTINGS_FILE);
    if !path.exists() {
        return SearchEngines::builtin();
    }

    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) => {
            log::warn!("Could not read {}: {error}", path.display());
            return SearchEngines::builtin();
        },
    };
    match parse_search_settings(&text) {
        Ok(engines) => engines,
        Err(error) => {
            log::warn!(
                "Could not parse {}: {error}. Using built-in search engines.",
                path.display()
            );
            SearchEngines::builtin()
        },
    }
}

/// Parse a `search.json` document.
///
/// ```json
/// {
///   "version": 1,
///   "engines": [
///     { "id": "ddg", "order": 1 },
///     { "id": "google", "order": 2 },
///     {
///       "id": "ecosia",
///       "name": "Ecosia",
///       "url": "https://www.ecosia.org/search?q=%s"
///     }
///   ],
///   "defaultEngineId": "",
///   "appDefaultEngineId": "ddg",
///   "useSavedOrder": false
/// }
/// ```
///
/// An empty `defaultEngineId` means "use `appDefaultEngineId`". A built-in id
/// with no `url` uses the application template. A `url` makes the entry a
/// user engine; `{searchTerms}` is rewritten to `%s`.
pub(crate) fn parse_search_settings(json: &str) -> Result<SearchEngines, String> {
    let value: Value = serde_json::from_str(json).map_err(|error| error.to_string())?;
    let Some(object) = value.as_object() else {
        return Err("search settings must be a JSON object".to_owned());
    };
    let Some(entries) = object.get("engines").and_then(Value::as_array) else {
        return Err("search settings have no engines array".to_owned());
    };

    let mut engines: Vec<SearchEngine> = Vec::new();
    for entry in entries {
        if let Some(engine) = engine_from_value(entry) {
            if engines
                .iter()
                .any(|existing: &SearchEngine| existing.id == engine.id)
            {
                log::warn!("Ignoring duplicate search engine {}", engine.id);
                continue;
            }
            engines.push(engine);
        }
    }
    if engines.is_empty() {
        return Err("search settings contain no usable engines".to_owned());
    }

    let use_saved_order = object
        .get("useSavedOrder")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if use_saved_order {
        engines.sort_by(|left, right| match (left.order, right.order) {
            (Some(left), Some(right)) => left.cmp(&right),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        });
    }

    let default_engine_id = object
        .get("defaultEngineId")
        .and_then(Value::as_str)
        .unwrap_or("");
    let app_default_engine_id = object
        .get("appDefaultEngineId")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty());
    let default_id = select_default(&engines, default_engine_id, app_default_engine_id);

    Ok(SearchEngines {
        engines,
        default_id,
    })
}

fn engine_from_value(value: &Value) -> Option<SearchEngine> {
    let id = string_field(value, "id").filter(|id| !id.is_empty())?;
    let order = value
        .get("order")
        .and_then(Value::as_u64)
        .and_then(|order| u32::try_from(order).ok());
    let alias = string_field(value, "alias").filter(|alias| !alias.is_empty());
    let name_override = string_field(value, "name").filter(|name| !name.is_empty());

    if let Some(url) = string_field(value, "url") {
        let Some(url) = normalize_search_url(&url) else {
            log::warn!("Ignoring search engine {id}: url has no %s or {{searchTerms}} placeholder");
            return None;
        };
        let name = name_override
            .or_else(|| builtin_by_id(&id).map(|engine| engine.name.to_owned()))
            .unwrap_or_else(|| id.clone());
        return Some(SearchEngine {
            id,
            name,
            url,
            alias,
            order,
            builtin: false,
        });
    }

    let Some(builtin) = builtin_by_id(&id) else {
        log::warn!("Ignoring search engine {id}: unknown built-in id and no url");
        return None;
    };
    Some(SearchEngine {
        id,
        name: name_override.unwrap_or_else(|| builtin.name.to_owned()),
        url: builtin.url.to_owned(),
        alias,
        order,
        builtin: true,
    })
}

fn select_default(
    engines: &[SearchEngine],
    default_engine_id: &str,
    app_default_engine_id: Option<&str>,
) -> String {
    if !default_engine_id.is_empty() && engines.iter().any(|engine| engine.id == default_engine_id)
    {
        return default_engine_id.to_owned();
    }
    if let Some(app_default) = app_default_engine_id &&
        engines.iter().any(|engine| engine.id == app_default)
    {
        return app_default.to_owned();
    }
    if engines
        .iter()
        .any(|engine| engine.id == APP_DEFAULT_ENGINE_ID)
    {
        return APP_DEFAULT_ENGINE_ID.to_owned();
    }
    engines[0].id.clone()
}

fn builtin_by_id(id: &str) -> Option<&'static BuiltinEngine> {
    BUILTIN_ENGINES.iter().find(|engine| engine.id == id)
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn apply_search_change(
    engines: &mut SearchEngines,
    params: &[(String, String)],
) -> Result<(), String> {
    let value = |key: &str| {
        params
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    };
    if let Some(id) = value("default") {
        engines.set_default(id)
    } else if let Some(id) = value("remove") {
        engines.remove(id)
    } else if let Some(id) = value("addBuiltin") {
        engines.add_builtin(id)
    } else if value("name").is_some() || value("url").is_some() {
        engines.add_user(value("name").unwrap_or(""), value("url").unwrap_or(""))
    } else {
        Err("Unknown search settings request".to_owned())
    }
}

fn save_search_engines(config_dir: &Path, engines: &SearchEngines) -> Result<(), String> {
    fs::create_dir_all(config_dir).map_err(|error| error.to_string())?;
    let text = serde_json::to_string_pretty(&settings_value(engines))
        .map_err(|error| error.to_string())?;
    fs::write(config_dir.join(SEARCH_SETTINGS_FILE), format!("{text}\n"))
        .map_err(|error| error.to_string())
}

fn settings_value(engines: &SearchEngines) -> Value {
    let entries: Vec<Value> = engines
        .engines
        .iter()
        .enumerate()
        .map(|(index, engine)| {
            let mut entry = serde_json::Map::new();
            entry.insert("id".into(), Value::String(engine.id.clone()));
            entry.insert("name".into(), Value::String(engine.name.clone()));
            entry.insert("order".into(), Value::from(index as u64 + 1));
            if let Some(alias) = &engine.alias {
                entry.insert("alias".into(), Value::String(alias.clone()));
            }
            if !engine.builtin {
                entry.insert("url".into(), Value::String(engine.url.clone()));
            }
            Value::Object(entry)
        })
        .collect();
    serde_json::json!({
        "version": 1,
        "useSavedOrder": true,
        "appDefaultEngineId": APP_DEFAULT_ENGINE_ID,
        "defaultEngineId": engines.default_id,
        "engines": entries,
    })
}

fn api_json(engines: &SearchEngines, error: Option<&str>) -> String {
    let configured = engines
        .engines
        .iter()
        .map(|engine| {
            serde_json::json!({
                "id": engine.id,
                "name": engine.name,
                "url": engine.url,
                "builtin": engine.builtin,
            })
        })
        .collect::<Vec<_>>();
    let available = BUILTIN_ENGINES
        .iter()
        .filter(|builtin| !engines.engines.iter().any(|engine| engine.id == builtin.id))
        .map(|builtin| serde_json::json!({ "id": builtin.id, "name": builtin.name }))
        .collect::<Vec<_>>();
    let mut value = serde_json::json!({
        "defaultId": engines.default_id,
        "engines": configured,
        "available": available,
    });
    if let Some(error) = error {
        value["error"] = Value::String(error.to_owned());
    }
    serde_json::to_string(&value)
        .unwrap_or_else(|_| "{\"error\":\"Could not encode search engines\"}".to_owned())
}

fn slug(name: &str) -> String {
    let mut id = String::new();
    for character in name.chars() {
        if character.is_ascii_alphanumeric() {
            id.push(character.to_ascii_lowercase());
        } else if !id.is_empty() && !id.ends_with('-') {
            id.push('-');
        }
    }
    let id = id.trim_matches('-');
    if id.is_empty() {
        "engine".to_owned()
    } else {
        id.to_owned()
    }
}

fn unique_engine_id(engines: &SearchEngines, base: &str) -> String {
    if !engines.engines.iter().any(|engine| engine.id == base) {
        return base.to_owned();
    }
    let mut suffix = 2u32;
    loop {
        let candidate = format!("{base}-{suffix}");
        if !engines.engines.iter().any(|engine| engine.id == candidate) {
            return candidate;
        }
        suffix += 1;
    }
}

/// Accept Aether's `%s` and Firefox's `{searchTerms}`.
fn normalize_search_url(url: &str) -> Option<String> {
    let url = url.trim();
    if url.is_empty() {
        return None;
    }
    let normalized = url.replace("{searchTerms}", "%s");
    if !normalized.contains("%s") {
        return None;
    }
    let parsed = url::Url::parse(&normalized.replace("%s", "test")).ok()?;
    matches!(parsed.scheme(), "http" | "https").then_some(normalized)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::*;

    fn temp_config_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("aether-search-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn builtin_default_is_duckduckgo() {
        let engines = SearchEngines::builtin();
        assert_eq!(engines.default_engine().id, "ddg");
        assert_eq!(
            engines.default_engine().url,
            "https://duckduckgo.com/html/?q=%s"
        );
        assert_eq!(
            SearchEngines::builtin_catalog().len(),
            engines.engines().len()
        );
        assert!(engines.engines().iter().all(|engine| engine.builtin));
        assert_eq!(
            engines.additional_engines().count(),
            engines.engines().len() - 1
        );
    }

    #[test]
    fn empty_default_engine_id_uses_app_default() {
        let engines = parse_search_settings(
            r#"{
                "version": 1,
                "engines": [
                    { "id": "google" },
                    { "id": "ddg" }
                ],
                "defaultEngineId": "",
                "appDefaultEngineId": "google"
            }"#,
        )
        .unwrap();
        assert_eq!(engines.default_engine().id, "google");
        assert_eq!(
            engines.default_engine().url,
            "https://www.google.com/search?q=%s"
        );
        assert_eq!(
            engines
                .additional_engines()
                .map(|engine| engine.id.as_str())
                .collect::<Vec<_>>(),
            ["ddg"]
        );
    }

    #[test]
    fn default_engine_id_overrides_app_default() {
        let engines = parse_search_settings(
            r#"{
                "engines": [
                    { "id": "google" },
                    { "id": "ddg" }
                ],
                "defaultEngineId": "ddg",
                "appDefaultEngineId": "google"
            }"#,
        )
        .unwrap();
        assert_eq!(engines.default_engine().id, "ddg");
    }

    #[test]
    fn user_engine_is_loaded_alongside_builtins() {
        let engines = parse_search_settings(
            r#"{
                "engines": [
                    { "id": "ddg" },
                    { "id": "google" },
                    { "id": "bing" },
                    { "id": "startpage" },
                    {
                        "id": "ecosia",
                        "name": "Ecosia",
                        "url": "https://www.ecosia.org/search?method=index&q=%s"
                    }
                ],
                "defaultEngineId": "ddg"
            }"#,
        )
        .unwrap();

        assert_eq!(
            engines
                .engines()
                .iter()
                .map(|engine| engine.id.as_str())
                .collect::<Vec<_>>(),
            ["ddg", "google", "bing", "startpage", "ecosia"]
        );
        let ecosia = engines
            .engines()
            .iter()
            .find(|engine| engine.id == "ecosia")
            .expect("Ecosia is configured");
        assert!(!ecosia.builtin);
        assert_eq!(ecosia.name, "Ecosia");
        assert_eq!(ecosia.alias, None);
        assert_eq!(
            ecosia.url,
            "https://www.ecosia.org/search?method=index&q=%s"
        );
        assert_eq!(engines.default_engine().id, "ddg");
        assert!(
            engines
                .additional_engines()
                .any(|engine| engine.id == "ecosia")
        );
    }

    #[test]
    fn unknown_engine_without_url_is_ignored() {
        let engines = parse_search_settings(
            r#"{
                "engines": [
                    { "id": "perplexity" },
                    { "id": "bing" },
                    { "id": "bing" }
                ],
                "defaultEngineId": "perplexity",
                "appDefaultEngineId": "bing"
            }"#,
        )
        .unwrap();
        assert_eq!(engines.engines().len(), 1);
        assert_eq!(engines.default_engine().id, "bing");
    }

    #[test]
    fn settings_without_usable_engines_are_rejected() {
        let error = parse_search_settings(r#"{ "engines": [{ "id": "nope" }] }"#).unwrap_err();
        assert!(error.contains("no usable engines"));
        assert!(parse_search_settings("[]").is_err());
        assert!(parse_search_settings("{").is_err());
    }

    #[test]
    fn missing_or_corrupt_file_uses_builtin_engines() {
        assert_eq!(
            load_search_engines(None).default_engine().id,
            APP_DEFAULT_ENGINE_ID
        );

        let missing = temp_config_dir("missing");
        assert_eq!(
            load_search_engines(Some(&missing)).default_engine().id,
            "ddg"
        );

        let corrupt = temp_config_dir("corrupt");
        fs::write(corrupt.join(SEARCH_SETTINGS_FILE), "not json").unwrap();
        assert_eq!(
            load_search_engines(Some(&corrupt)).default_engine().id,
            "ddg"
        );

        let configured = temp_config_dir("configured");
        fs::write(
            configured.join(SEARCH_SETTINGS_FILE),
            r#"{ "engines": [{ "id": "bing" }], "defaultEngineId": "bing" }"#,
        )
        .unwrap();
        let loaded = load_search_engines(Some(&configured));
        assert_eq!(loaded.default_engine().id, "bing");
        assert_eq!(loaded.engines().len(), 1);

        let _ = fs::remove_dir_all(missing);
        let _ = fs::remove_dir_all(corrupt);
        let _ = fs::remove_dir_all(configured);
    }

    #[test]
    fn default_can_change_and_the_last_engine_cannot_be_removed() {
        let mut engines = SearchEngines::builtin();
        engines.set_default("google").unwrap();
        assert_eq!(engines.default_engine().id, "google");
        assert!(engines.set_default("missing").is_err());

        engines.remove("ddg").unwrap();
        assert!(engines.engines().iter().all(|engine| engine.id != "ddg"));
        while engines.engines().len() > 1 {
            let id = engines.engines()[0].id.clone();
            engines.remove(&id).unwrap();
        }
        let last = engines.engines()[0].id.clone();
        assert!(engines.remove(&last).is_err());
        assert_eq!(engines.default_engine().id, last);
    }

    #[test]
    fn removing_the_default_selects_another_engine() {
        let mut engines = SearchEngines::builtin();
        engines.remove("ddg").unwrap();
        assert_eq!(engines.default_engine().id, "google");
    }

    #[test]
    fn user_engine_can_be_added_and_saved() {
        let mut engines = SearchEngines::builtin();
        engines
            .add_user(
                "Ecosia",
                "https://www.ecosia.org/search?method=index&q={searchTerms}",
            )
            .unwrap();
        engines.set_default("ecosia").unwrap();
        let ecosia = engines
            .engines()
            .iter()
            .find(|engine| engine.id == "ecosia")
            .unwrap();
        assert!(!ecosia.builtin);
        assert_eq!(
            ecosia.url,
            "https://www.ecosia.org/search?method=index&q=%s"
        );

        let dir = temp_config_dir("save-ecosia");
        save_search_engines(&dir, &engines).unwrap();
        let loaded = load_search_engines(Some(&dir));
        assert_eq!(loaded.default_engine().id, "ecosia");
        assert_eq!(
            loaded.default_engine().url,
            "https://www.ecosia.org/search?method=index&q=%s"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn removed_builtin_can_be_added_again() {
        let mut engines = SearchEngines::builtin();
        engines.remove("bing").unwrap();
        engines.add_builtin("bing").unwrap();
        assert!(engines.engines().iter().any(|engine| engine.id == "bing"));
        assert!(engines.add_builtin("bing").is_err());
        assert!(engines.add_user("  ", "https://example.com/?q=%s").is_err());
        assert!(engines.add_user("Example", "https://example.com/").is_err());
    }
}
