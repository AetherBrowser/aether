/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Homepage, new-window, and new-tab preferences.
//!
//! Firefox stores these in the profile prefs:
//! - `browser.startup.homepage` — `about:home`, `about:blank`, or a custom URL
//! - `browser.startup.page` — `0` blank (deprecated), `1` the homepage
//! - `browser.newtabpage.enabled` — Firefox Home vs a blank new tab
//! - `browser.newtabpage.activity-stream.showSearch`
//! - `browser.newtabpage.activity-stream.logowordmark.alwaysVisible`
//!
//! Aether keeps the same names in plain JSON at `{config_dir}/home.json`.
//! `about:home` is the Aether home page (`about:newtab`). Changes from
//! `about:settings#home` are written back to that file.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::{Map, Value};
use url::Url;

/// User settings file, next to `prefs.json` in the config directory.
pub(crate) const HOME_SETTINGS_FILE: &str = "home.json";

/// Stored value for the Aether home page. Resolved to [`AETHER_HOME_URL`].
const ABOUT_HOME: &str = "about:home";

/// Page shown for the Aether home page.
pub(crate) const AETHER_HOME_URL: &str = "about:newtab";
pub(crate) const BLANK_URL: &str = "about:blank";

/// `browser.startup.page`: show a blank page. Deprecated in Firefox; a blank
/// homepage is normally `browser.startup.homepage = about:blank`.
const STARTUP_BLANK: i64 = 0;
const STARTUP_HOMEPAGE: i64 = 1;

const PREF_HOMEPAGE: &str = "browser.startup.homepage";
const PREF_STARTUP_PAGE: &str = "browser.startup.page";
const PREF_NEWTAB_ENABLED: &str = "browser.newtabpage.enabled";
const PREF_SHOW_SEARCH: &str = "browser.newtabpage.activity-stream.showSearch";
const PREF_SHOW_LOGO: &str = "browser.newtabpage.activity-stream.logowordmark.alwaysVisible";

/// What a new window opens. Matches the Homepage and new windows menu in Firefox.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NewWindowChoice {
    /// Aether home page (`about:home`).
    Home,
    /// The URL in [`HomeSettings::custom_url`].
    Url,
    /// `about:blank`.
    Blank,
}

/// Homepage preferences for this profile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HomeSettings {
    pub new_windows: NewWindowChoice,
    /// Custom homepage. Used when [`Self::new_windows`] is [`NewWindowChoice::Url`].
    pub custom_url: String,
    /// `browser.startup.page`. `0` means blank, `1` means the homepage.
    startup_page: i64,
    /// `browser.newtabpage.enabled`. `false` opens a blank new tab.
    new_tabs_home: bool,
    pub show_search: bool,
    pub show_logo: bool,
}

impl Default for HomeSettings {
    fn default() -> Self {
        Self {
            new_windows: NewWindowChoice::Home,
            custom_url: String::new(),
            startup_page: STARTUP_HOMEPAGE,
            new_tabs_home: true,
            show_search: true,
            show_logo: true,
        }
    }
}

impl HomeSettings {
    /// URL string opened in a new window and by the Home button.
    pub(crate) fn new_window_url(&self) -> String {
        match self.new_windows {
            NewWindowChoice::Home => AETHER_HOME_URL.to_owned(),
            NewWindowChoice::Blank => BLANK_URL.to_owned(),
            NewWindowChoice::Url => self.custom_url.clone(),
        }
    }

    /// URL string opened in a new tab.
    pub(crate) fn new_tab_url(&self) -> String {
        if self.new_tabs_home {
            AETHER_HOME_URL.to_owned()
        } else {
            BLANK_URL.to_owned()
        }
    }

    fn set_new_windows_home(&mut self) {
        self.new_windows = NewWindowChoice::Home;
        self.custom_url.clear();
        self.leave_blank_startup();
    }

    fn set_new_windows_blank(&mut self) {
        self.new_windows = NewWindowChoice::Blank;
        self.custom_url.clear();
        self.leave_blank_startup();
    }

    fn set_new_windows_url(&mut self, url: &str) -> Result<(), String> {
        let url = normalize_homepage_url(url)?;
        self.new_windows = NewWindowChoice::Url;
        self.custom_url = url;
        self.leave_blank_startup();
        Ok(())
    }

    fn leave_blank_startup(&mut self) {
        if self.startup_page == STARTUP_BLANK {
            self.startup_page = STARTUP_HOMEPAGE;
        }
    }
}

struct InstalledHomeSettings {
    config_dir: Option<PathBuf>,
    settings: HomeSettings,
}

static INSTALLED: Mutex<Option<InstalledHomeSettings>> = Mutex::new(None);

/// Remember the settings loaded at startup so `about:settings#home` and new
/// tabs share one copy.
pub(crate) fn install_home_settings(config_dir: Option<PathBuf>, settings: HomeSettings) {
    *INSTALLED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(InstalledHomeSettings {
        config_dir,
        settings,
    });
}

fn installed_or_default() -> HomeSettings {
    INSTALLED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .as_ref()
        .map(|state| state.settings.clone())
        .unwrap_or_default()
}

/// Page opened by a new tab, from the installed settings.
pub(crate) fn new_tab_url() -> Url {
    parse_page_url(&installed_or_default().new_tab_url())
}

/// Page opened by a new window and the Home button, from the installed settings.
pub(crate) fn new_window_url() -> Url {
    parse_page_url(&installed_or_default().new_window_url())
}

fn parse_page_url(url: &str) -> Url {
    Url::parse(url)
        .unwrap_or_else(|_| Url::parse(AETHER_HOME_URL).expect("Aether home URL should parse"))
}

/// Answer `about:home`. A query string applies one change and saves it.
pub(crate) fn handle_home_request(query: Option<&str>) -> String {
    let mut installed = INSTALLED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(state) = installed.as_mut() else {
        return api_json(&HomeSettings::default(), None);
    };

    let mut error = None;
    if let Some(query) = query.filter(|query| !query.is_empty()) {
        let params: Vec<(String, String)> = url::form_urlencoded::parse(query.as_bytes())
            .into_owned()
            .collect();
        let mut updated = state.settings.clone();
        match apply_home_change(&mut updated, &params) {
            Ok(()) => match state
                .config_dir
                .as_deref()
                .map(|dir| save_home_settings(dir, &updated))
                .transpose()
            {
                Ok(_) => state.settings = updated,
                Err(save_error) => error = Some(save_error),
            },
            Err(change_error) => error = Some(change_error),
        }
    }

    api_json(&state.settings, error.as_deref())
}

/// Load `home.json` from `config_dir`.
///
/// A missing directory, a missing file, or a file that cannot be read falls
/// back to [`HomeSettings::default`].
pub(crate) fn load_home_settings(config_dir: Option<&Path>) -> HomeSettings {
    let Some(config_dir) = config_dir else {
        return HomeSettings::default();
    };
    let path = config_dir.join(HOME_SETTINGS_FILE);
    if !path.exists() {
        return HomeSettings::default();
    }

    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) => {
            log::warn!("Could not read {}: {error}", path.display());
            return HomeSettings::default();
        },
    };
    match parse_home_settings(&text) {
        Ok(settings) => settings,
        Err(error) => {
            log::warn!(
                "Could not parse {}: {error}. Using default homepage settings.",
                path.display()
            );
            HomeSettings::default()
        },
    }
}

/// Parse a `home.json` document. Missing keys use the Firefox defaults.
pub(crate) fn parse_home_settings(json: &str) -> Result<HomeSettings, String> {
    let value: Value = serde_json::from_str(json).map_err(|error| error.to_string())?;
    let Some(object) = value.as_object() else {
        return Err("home settings must be a JSON object".to_owned());
    };

    let startup_page = object
        .get(PREF_STARTUP_PAGE)
        .and_then(Value::as_i64)
        .unwrap_or(STARTUP_HOMEPAGE);
    let homepage = object
        .get(PREF_HOMEPAGE)
        .and_then(Value::as_str)
        .unwrap_or(ABOUT_HOME);
    let (new_windows, custom_url) = choice_from_homepage(homepage, startup_page);

    Ok(HomeSettings {
        new_windows,
        custom_url,
        startup_page,
        new_tabs_home: object
            .get(PREF_NEWTAB_ENABLED)
            .and_then(Value::as_bool)
            .unwrap_or(true),
        show_search: object
            .get(PREF_SHOW_SEARCH)
            .and_then(Value::as_bool)
            .unwrap_or(true),
        show_logo: object
            .get(PREF_SHOW_LOGO)
            .and_then(Value::as_bool)
            .unwrap_or(true),
    })
}

fn choice_from_homepage(homepage: &str, startup_page: i64) -> (NewWindowChoice, String) {
    let homepage = homepage.trim();
    if startup_page == STARTUP_BLANK || is_blank_homepage(homepage) {
        return (NewWindowChoice::Blank, String::new());
    }
    if is_aether_home(homepage) {
        return (NewWindowChoice::Home, String::new());
    }
    match normalize_homepage_url(homepage) {
        Ok(url) => (NewWindowChoice::Url, url),
        Err(error) => {
            log::warn!("Ignoring homepage {homepage:?}: {error}");
            (NewWindowChoice::Home, String::new())
        },
    }
}

fn is_aether_home(homepage: &str) -> bool {
    matches!(homepage, ABOUT_HOME | AETHER_HOME_URL)
}

fn is_blank_homepage(homepage: &str) -> bool {
    matches!(homepage, "" | BLANK_URL)
}

fn apply_home_change(
    settings: &mut HomeSettings,
    params: &[(String, String)],
) -> Result<(), String> {
    let value = |key: &str| {
        params
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    };

    let mut applied = false;
    if let Some(mode) = value("newWindows") {
        applied = true;
        match mode {
            "home" => settings.set_new_windows_home(),
            "blank" => settings.set_new_windows_blank(),
            "url" => settings.set_new_windows_url(value("homepage").unwrap_or(""))?,
            _ => return Err("Unknown new window setting".to_owned()),
        }
    } else if let Some(url) = value("homepage") {
        applied = true;
        settings.set_new_windows_url(url)?;
    }

    if let Some(mode) = value("newTabs") {
        applied = true;
        match mode {
            "home" => settings.new_tabs_home = true,
            "blank" => settings.new_tabs_home = false,
            _ => return Err("Unknown new tab setting".to_owned()),
        }
    }

    if let Some(flag) = value("showSearch") {
        applied = true;
        settings.show_search = parse_bool(flag)?;
    }
    if let Some(flag) = value("showLogo") {
        applied = true;
        settings.show_logo = parse_bool(flag)?;
    }

    if applied {
        Ok(())
    } else {
        Err("Unknown home settings request".to_owned())
    }
}

fn parse_bool(value: &str) -> Result<bool, String> {
    match value {
        "true" | "1" => Ok(true),
        "false" | "0" => Ok(false),
        _ => Err(format!("Expected true or false, got {value}")),
    }
}

fn normalize_homepage_url(raw: &str) -> Result<String, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("Enter a homepage URL".to_owned());
    }
    if let Ok(url) = Url::parse(raw) {
        return accepted_homepage(url);
    }
    if !raw.contains(' ') &&
        let Ok(url) = Url::parse(&format!("https://{raw}"))
    {
        return accepted_homepage(url);
    }
    Err("Enter a valid homepage URL".to_owned())
}

fn accepted_homepage(url: Url) -> Result<String, String> {
    if matches!(url.scheme(), "http" | "https" | "file") {
        Ok(url.to_string())
    } else {
        Err("Homepage URL must be http, https, or file".to_owned())
    }
}

fn save_home_settings(config_dir: &Path, settings: &HomeSettings) -> Result<(), String> {
    fs::create_dir_all(config_dir).map_err(|error| error.to_string())?;
    let text = serde_json::to_string_pretty(&settings_value(settings))
        .map_err(|error| error.to_string())?;
    fs::write(config_dir.join(HOME_SETTINGS_FILE), format!("{text}\n"))
        .map_err(|error| error.to_string())
}

fn settings_value(settings: &HomeSettings) -> Value {
    let homepage = match settings.new_windows {
        NewWindowChoice::Home => ABOUT_HOME,
        NewWindowChoice::Blank => BLANK_URL,
        NewWindowChoice::Url => settings.custom_url.as_str(),
    };
    let mut object = Map::new();
    object.insert(PREF_HOMEPAGE.into(), Value::String(homepage.to_owned()));
    object.insert(PREF_STARTUP_PAGE.into(), Value::from(settings.startup_page));
    object.insert(
        PREF_NEWTAB_ENABLED.into(),
        Value::Bool(settings.new_tabs_home),
    );
    object.insert(PREF_SHOW_SEARCH.into(), Value::Bool(settings.show_search));
    object.insert(PREF_SHOW_LOGO.into(), Value::Bool(settings.show_logo));
    Value::Object(object)
}

fn api_json(settings: &HomeSettings, error: Option<&str>) -> String {
    let new_windows = match settings.new_windows {
        NewWindowChoice::Home => "home",
        NewWindowChoice::Url => "url",
        NewWindowChoice::Blank => "blank",
    };
    let homepage = if settings.new_windows == NewWindowChoice::Url {
        settings.custom_url.as_str()
    } else {
        ""
    };
    let mut value = serde_json::json!({
        "newWindows": new_windows,
        "homepage": homepage,
        "newTabs": if settings.new_tabs_home { "home" } else { "blank" },
        "showSearch": settings.show_search,
        "showLogo": settings.show_logo,
    });
    if let Some(error) = error {
        value["error"] = Value::String(error.to_owned());
    }
    serde_json::to_string(&value)
        .unwrap_or_else(|_| "{\"error\":\"Could not encode home settings\"}".to_owned())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::*;

    fn temp_config_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("aether-home-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temp config dir");
        dir
    }

    #[test]
    fn defaults_match_firefox_home() {
        let settings = HomeSettings::default();
        assert_eq!(settings.new_window_url(), AETHER_HOME_URL);
        assert_eq!(settings.new_tab_url(), AETHER_HOME_URL);
        assert!(settings.show_search);
        assert!(settings.show_logo);

        let stored = settings_value(&settings);
        assert_eq!(stored[PREF_HOMEPAGE], ABOUT_HOME);
        assert_eq!(stored[PREF_STARTUP_PAGE], STARTUP_HOMEPAGE);
        assert_eq!(stored[PREF_NEWTAB_ENABLED], true);
        assert_eq!(stored[PREF_SHOW_SEARCH], true);
        assert_eq!(stored[PREF_SHOW_LOGO], true);
    }

    #[test]
    fn firefox_blank_and_custom_homepage_values_round_trip() {
        let blank = parse_home_settings(
            r#"{
                "browser.startup.homepage": "about:blank",
                "browser.startup.page": 1,
                "browser.newtabpage.enabled": false,
                "browser.newtabpage.activity-stream.showSearch": false,
                "browser.newtabpage.activity-stream.logowordmark.alwaysVisible": false
            }"#,
        )
        .unwrap();
        assert_eq!(blank.new_windows, NewWindowChoice::Blank);
        assert_eq!(blank.new_window_url(), BLANK_URL);
        assert_eq!(blank.new_tab_url(), BLANK_URL);
        assert!(!blank.show_search);
        assert!(!blank.show_logo);

        let custom =
            parse_home_settings(r#"{ "browser.startup.homepage": "https://example.com/start" }"#)
                .unwrap();
        assert_eq!(custom.new_windows, NewWindowChoice::Url);
        assert_eq!(custom.new_window_url(), "https://example.com/start");
        assert_eq!(custom.new_tab_url(), AETHER_HOME_URL);

        let deprecated = parse_home_settings(r#"{ "browser.startup.page": 0 }"#).unwrap();
        assert_eq!(deprecated.new_windows, NewWindowChoice::Blank);
    }

    #[test]
    fn stored_home_values_open_the_aether_home_page() {
        for homepage in [ABOUT_HOME, AETHER_HOME_URL] {
            let settings = parse_home_settings(&format!(
                r#"{{ "browser.startup.homepage": "{homepage}" }}"#
            ))
            .unwrap();
            assert_eq!(settings.new_windows, NewWindowChoice::Home);
            assert_eq!(settings.new_window_url(), AETHER_HOME_URL);
        }
    }

    #[test]
    fn changes_are_saved_with_firefox_pref_names() {
        let dir = temp_config_dir("save");
        let mut settings = HomeSettings::default();
        apply_home_change(
            &mut settings,
            &[
                ("newWindows".into(), "url".into()),
                ("homepage".into(), "example.com".into()),
                ("newTabs".into(), "blank".into()),
                ("showSearch".into(), "false".into()),
                ("showLogo".into(), "true".into()),
            ],
        )
        .unwrap();
        save_home_settings(&dir, &settings).unwrap();

        let loaded = load_home_settings(Some(&dir));
        assert_eq!(loaded.new_windows, NewWindowChoice::Url);
        assert_eq!(loaded.custom_url, "https://example.com/");
        assert!(!loaded.new_tabs_home);
        assert!(!loaded.show_search);
        assert!(loaded.show_logo);

        let text = fs::read_to_string(dir.join(HOME_SETTINGS_FILE)).unwrap();
        assert!(text.contains("\"browser.startup.homepage\": \"https://example.com/\""));
        assert!(text.contains("\"browser.newtabpage.enabled\": false"));
        assert!(text.contains("\"browser.newtabpage.activity-stream.showSearch\": false"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_uses_defaults_and_rejects_bad_urls() {
        assert_eq!(load_home_settings(None).new_windows, NewWindowChoice::Home);
        let missing = temp_config_dir("missing");
        assert_eq!(
            load_home_settings(Some(&missing)).new_tab_url(),
            AETHER_HOME_URL
        );
        let _ = fs::remove_dir_all(&missing);

        let mut settings = HomeSettings::default();
        let error = apply_home_change(
            &mut settings,
            &[
                ("newWindows".into(), "url".into()),
                ("homepage".into(), "".into()),
            ],
        )
        .unwrap_err();
        assert!(error.contains("URL"));
        assert_eq!(settings.new_windows, NewWindowChoice::Home);
        assert!(apply_home_change(&mut settings, &[("newTabs".into(), "nope".into())]).is_err());
    }
}
