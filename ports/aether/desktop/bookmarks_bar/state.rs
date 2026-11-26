/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! When the windows show the bookmarks bar, saved in `bookmarks.json`, and what the bar holds.

use std::cell::{Cell, RefCell};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex, PoisonError};

use aether_places::BookmarkNode;
use log::warn;
use serde_json::{Value, json};
use servo::{EventLoopWaker, Opts};
use url::Url;

use crate::home::AETHER_HOME_URL;
use crate::window::new_tab_url;

const SETTINGS_FILE: &str = "bookmarks.json";

/// When the windows show the bookmarks bar.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum BarVisibility {
    #[default]
    Always,
    /// Only over the new tab page.
    NewTab,
    Never,
}

impl BarVisibility {
    pub(super) const ALL: [Self; 3] = [Self::Always, Self::NewTab, Self::Never];

    /// The saved value, as in `browser.toolbars.bookmarks.visibility` of Firefox.
    fn name(self) -> &'static str {
        match self {
            Self::Always => "always",
            Self::NewTab => "newtab",
            Self::Never => "never",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|visibility| visibility.name() == name)
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Always => "Always Show",
            Self::NewTab => "Only Show on New Tab",
            Self::Never => "Never Show",
        }
    }

    /// Whether a window whose active tab shows `url` shows the bar.
    pub(crate) fn shows_bar(self, url: Option<&Url>) -> bool {
        match self {
            Self::Always => true,
            Self::NewTab => {
                url.is_some_and(|url| url.as_str() == AETHER_HOME_URL || *url == new_tab_url())
            },
            Self::Never => false,
        }
    }
}

/// The visibility of the bar, which the bookmarks and settings pages change from other threads.
#[derive(Clone)]
pub(crate) struct BarVisibilitySetting {
    /// Where the visibility is saved, or `None` when it stays in memory.
    config_dir: Option<PathBuf>,
    visibility: Arc<Mutex<BarVisibility>>,
    waker: Box<dyn EventLoopWaker>,
}

impl BarVisibilitySetting {
    fn get(&self) -> BarVisibility {
        *self
            .visibility
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Saves `visibility`, and wakes the event loop to show it in the windows.
    fn set(&self, visibility: BarVisibility) {
        let mut current = self
            .visibility
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        *current = visibility;
        if let Some(config_dir) = &self.config_dir {
            save_visibility(config_dir, visibility);
        }
        self.waker.wake();
    }

    /// Answers `about:bookmarks-bar`, after applying its `visibility` parameter, if any.
    pub(crate) fn handle_request(&self, query: Option<&str>) -> String {
        let requested = url::form_urlencoded::parse(query.unwrap_or_default().as_bytes())
            .find(|(name, _)| name == "visibility");
        if let Some((_, name)) = requested {
            match BarVisibility::from_name(&name) {
                Some(visibility) => self.set(visibility),
                None => return json!({ "error": "This request is not valid." }).to_string(),
            }
        }
        json!({ "visibility": self.get().name() }).to_string()
    }
}

/// The content of the bookmarks bar, and when the windows show it.
pub(crate) struct BookmarksBarState {
    setting: BarVisibilitySetting,
    /// The visibility shown by the windows, taken from `setting` by [`Self::update_visibility`].
    visibility: Cell<BarVisibility>,
    tree: RefCell<Rc<Vec<BookmarkNode>>>,
}

impl BookmarksBarState {
    pub(crate) fn load(opts: &Opts, waker: Box<dyn EventLoopWaker>) -> Self {
        let config_dir = opts.config_dir.clone().filter(|_| !opts.temporary_storage);
        let visibility = config_dir
            .as_deref()
            .map(load_visibility)
            .unwrap_or_default();
        Self {
            setting: BarVisibilitySetting {
                config_dir,
                visibility: Arc::new(Mutex::new(visibility)),
                waker,
            },
            visibility: Cell::new(visibility),
            tree: Default::default(),
        }
    }

    /// The setting, for the pages that change it.
    pub(crate) fn setting(&self) -> BarVisibilitySetting {
        self.setting.clone()
    }

    pub(crate) fn visibility(&self) -> BarVisibility {
        self.visibility.get()
    }

    pub(crate) fn set_visibility(&self, visibility: BarVisibility) {
        self.setting.set(visibility);
    }

    /// Takes the visibility last set, and returns whether it changed.
    pub(crate) fn update_visibility(&self) -> bool {
        let visibility = self.setting.get();
        self.visibility.replace(visibility) != visibility
    }

    /// As Ctrl+Shift+B in Firefox: hides the bar where it shows, and shows it everywhere otherwise.
    pub(crate) fn toggle(&self, url: Option<&Url>) {
        let visibility = if self.visibility().shows_bar(url) {
            BarVisibility::Never
        } else {
            BarVisibility::Always
        };
        self.set_visibility(visibility);
    }

    pub(crate) fn tree(&self) -> Rc<Vec<BookmarkNode>> {
        self.tree.borrow().clone()
    }

    pub(crate) fn set_tree(&self, tree: Vec<BookmarkNode>) {
        *self.tree.borrow_mut() = Rc::new(tree);
    }
}

fn load_visibility(config_dir: &Path) -> BarVisibility {
    let path = config_dir.join(SETTINGS_FILE);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == ErrorKind::NotFound => return BarVisibility::default(),
        Err(error) => {
            warn!("Could not read {}: {error}", path.display());
            return BarVisibility::default();
        },
    };
    let visibility = serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|value| BarVisibility::from_name(value.get("barVisibility")?.as_str()?));
    visibility.unwrap_or_else(|| {
        warn!("Ignoring {}: no valid barVisibility", path.display());
        BarVisibility::default()
    })
}

fn save_visibility(config_dir: &Path, visibility: BarVisibility) {
    let text = json!({ "version": 1, "barVisibility": visibility.name() });
    let written = fs::create_dir_all(config_dir)
        .and_then(|()| fs::write(config_dir.join(SETTINGS_FILE), format!("{text:#}\n")));
    if let Err(error) = written {
        warn!("Could not save the bookmarks bar settings: {error}");
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use servo::{EventLoopWaker, Opts};
    use url::Url;

    use super::{BarVisibility, BookmarksBarState, load_visibility, save_visibility};

    #[derive(Clone, Default)]
    struct CountingWaker(Arc<AtomicUsize>);

    impl EventLoopWaker for CountingWaker {
        fn clone_box(&self) -> Box<dyn EventLoopWaker> {
            Box::new(self.clone())
        }

        fn wake(&self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn state(config_dir: Option<&Path>) -> (BookmarksBarState, CountingWaker) {
        let opts = Opts {
            config_dir: config_dir.map(Path::to_owned),
            ..Default::default()
        };
        let waker = CountingWaker::default();
        (
            BookmarksBarState::load(&opts, Box::new(waker.clone())),
            waker,
        )
    }

    #[test]
    fn a_page_changes_the_visibility_of_the_windows() {
        let directory = tempfile::tempdir().unwrap();
        let (state, waker) = state(Some(directory.path()));
        let setting = state.setting();

        assert_eq!(setting.handle_request(None), r#"{"visibility":"always"}"#);
        assert_eq!(
            setting.handle_request(Some("visibility=newtab")),
            r#"{"visibility":"newtab"}"#
        );

        assert_eq!(waker.0.load(Ordering::SeqCst), 1);
        assert_eq!(state.visibility(), BarVisibility::Always);
        assert!(state.update_visibility());
        assert_eq!(state.visibility(), BarVisibility::NewTab);
        assert!(!state.update_visibility());
        assert_eq!(load_visibility(directory.path()), BarVisibility::NewTab);
    }

    #[test]
    fn an_unknown_visibility_is_refused() {
        let (state, waker) = state(None);

        assert_eq!(
            state.setting().handle_request(Some("visibility=sometimes")),
            r#"{"error":"This request is not valid."}"#
        );
        assert!(!state.update_visibility());
        assert_eq!(waker.0.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn the_shortcut_hides_a_shown_bar_and_shows_a_hidden_one() {
        let (state, _) = state(None);
        let page = Url::parse("https://servo.org/").unwrap();
        let new_tab = Url::parse("about:newtab").unwrap();
        let toggle = |url: &Url| {
            state.toggle(Some(url));
            state.update_visibility();
            state.visibility()
        };

        state.set_visibility(BarVisibility::NewTab);
        state.update_visibility();
        assert_eq!(toggle(&new_tab), BarVisibility::Never);
        assert_eq!(toggle(&page), BarVisibility::Always);
        assert_eq!(toggle(&page), BarVisibility::Never);
    }

    #[test]
    fn the_new_tab_mode_shows_the_bar_on_the_new_tab_page_only() {
        let new_tab = Url::parse("about:newtab").unwrap();
        let page = Url::parse("https://servo.org/").unwrap();

        assert!(BarVisibility::NewTab.shows_bar(Some(&new_tab)));
        assert!(!BarVisibility::NewTab.shows_bar(Some(&page)));
        assert!(!BarVisibility::NewTab.shows_bar(None));
        assert!(BarVisibility::Always.shows_bar(Some(&page)));
        assert!(!BarVisibility::Never.shows_bar(Some(&new_tab)));
    }

    #[test]
    fn the_bar_is_always_shown_without_settings_file() {
        let directory = tempfile::tempdir().unwrap();

        assert_eq!(load_visibility(directory.path()), BarVisibility::Always);
    }

    #[test]
    fn the_visibility_is_saved() {
        let directory = tempfile::tempdir().unwrap();

        for visibility in BarVisibility::ALL {
            save_visibility(directory.path(), visibility);
            assert_eq!(load_visibility(directory.path()), visibility);
        }
    }

    #[test]
    fn invalid_settings_show_the_bar() {
        let directory = tempfile::tempdir().unwrap();
        for text in [
            "not json",
            "{}",
            r#"{ "barVisibility": "sometimes" }"#,
            r#"{ "barVisibility": false }"#,
        ] {
            std::fs::write(directory.path().join("bookmarks.json"), text).unwrap();
            assert_eq!(
                load_visibility(directory.path()),
                BarVisibility::Always,
                "{text}"
            );
        }
    }
}
