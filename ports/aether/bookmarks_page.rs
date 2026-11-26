/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! The data of the `about:bookmarks` page.

use std::collections::HashMap;

use aether_places::{BookmarkEdit, BookmarkNode, BookmarksEditor};
use base64::Engine;
use base64::prelude::BASE64_STANDARD;
use serde_json::{Value, json};
use url::Url;

use crate::bookmarks::can_bookmark;

const INVALID_REQUEST: &str = "This request is not valid.";

pub(crate) struct BookmarksPage {
    editor: BookmarksEditor,
    can_open_tabs: bool,
}

impl BookmarksPage {
    pub(crate) fn new(editor: BookmarksEditor, can_open_tabs: bool) -> Self {
        Self {
            editor,
            can_open_tabs,
        }
    }

    /// Answers `about:bookmarks-data`, after applying the change described by `query`, if any.
    pub(crate) fn handle_request(
        &self,
        query: Option<&str>,
    ) -> impl Future<Output = String> + Send + 'static {
        let reply = parse_edit(query.unwrap_or_default()).map(|edit| match edit {
            Some(edit) => self.editor.edit(edit),
            None => self.editor.read(),
        });
        let can_open_tabs = self.can_open_tabs;
        async move {
            let tree = match reply {
                Ok(reply) => reply.await,
                Err(error) => Err(error),
            };
            match tree {
                Ok(tree) => {
                    let tree: Vec<_> = tree.iter().map(node_json).collect();
                    json!({ "tree": tree, "canOpenTabs": can_open_tabs }).to_string()
                },
                Err(error) => json!({ "error": error }).to_string(),
            }
        }
    }
}

fn parse_edit(query: &str) -> Result<Option<BookmarkEdit>, String> {
    let parameters: HashMap<_, _> = url::form_urlencoded::parse(query.as_bytes()).collect();
    let Some(action) = parameters.get("action") else {
        return Ok(None);
    };
    let text = |name: &str| {
        parameters
            .get(name)
            .map_or_else(String::new, |v| v.to_string())
    };
    let id = || required_number(parameters.get("id").map(|value| value.as_ref()));
    let parent = || optional_number(parameters.get("parent").map(|value| value.as_ref()));
    let edit = match action.as_ref() {
        "create-folder" => BookmarkEdit::CreateFolder {
            parent: parent()?,
            title: text("title"),
        },
        "update" => BookmarkEdit::Update {
            id: id()?,
            title: text("title"),
            url: parameters
                .get("url")
                .map(|address| parse_address(address))
                .transpose()?,
        },
        "move" => BookmarkEdit::Move {
            id: id()?,
            parent: parent()?,
            index: match parameters.get("index") {
                Some(index) => index.parse().map_err(|_| INVALID_REQUEST.to_owned())?,
                None => usize::MAX,
            },
        },
        "delete" => BookmarkEdit::Delete { id: id()? },
        _ => return Err(INVALID_REQUEST.to_owned()),
    };
    Ok(Some(edit))
}

fn required_number(value: Option<&str>) -> Result<i64, String> {
    value
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| INVALID_REQUEST.to_owned())
}

/// An empty value is the root of the tree.
fn optional_number(value: Option<&str>) -> Result<Option<i64>, String> {
    match value {
        None | Some("") => Ok(None),
        value => required_number(value).map(Some),
    }
}

fn parse_address(address: &str) -> Result<Url, String> {
    Url::parse(address.trim())
        .ok()
        .filter(can_bookmark)
        .ok_or_else(|| "Enter a complete address, such as https://example.com/.".to_owned())
}

fn node_json(node: &BookmarkNode) -> Value {
    let icon = node
        .icon
        .as_ref()
        .map(|png| format!("data:image/png;base64,{}", BASE64_STANDARD.encode(png)));
    let children: Vec<_> = node.children.iter().map(node_json).collect();
    json!({
        "id": node.id,
        "title": node.title,
        "url": node.url.as_ref().map(Url::as_str),
        "icon": icon,
        "children": children,
    })
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use aether_places::PlacesService;
    use futures::executor::block_on;
    use serde_json::{Value, json};
    use url::Url;

    use super::BookmarksPage;

    fn page(service: &PlacesService) -> BookmarksPage {
        BookmarksPage::new(service.bookmarks_editor(), true)
    }

    fn service() -> PlacesService {
        PlacesService::open_in_memory(Duration::MAX, || {})
    }

    fn response(page: &BookmarksPage, query: &str) -> Value {
        serde_json::from_str(&block_on(page.handle_request(Some(query)))).unwrap()
    }

    fn add_page(service: &PlacesService) {
        let url = Url::parse("https://servo.org/").unwrap();
        service.add_bookmark(url.clone(), Some("Servo".to_owned()), UNIX_EPOCH, 1);
        service.set_bookmark_icon(url, vec![1, 2, 3]);
    }

    #[test]
    fn the_tree_is_sent_with_its_icons_as_data_urls() {
        let service = service();
        add_page(&service);

        assert_eq!(
            response(&page(&service), ""),
            json!({
                "canOpenTabs": true,
                "tree": [{
                    "id": 1,
                    "title": "Servo",
                    "url": "https://servo.org/",
                    "icon": "data:image/png;base64,AQID",
                    "children": [],
                }],
            })
        );
    }

    #[test]
    fn an_empty_tree_tells_if_tabs_can_be_opened() {
        let service = service();
        let page = BookmarksPage::new(service.bookmarks_editor(), false);

        assert_eq!(
            response(&page, ""),
            json!({ "tree": [], "canOpenTabs": false })
        );
    }

    #[test]
    fn folders_are_created_in_the_root_or_in_a_folder() {
        let service = service();
        let page = page(&service);

        let first = response(&page, "action=create-folder&title=Rust&parent=");
        let id = first["tree"][0]["id"].as_i64().unwrap();
        let second = response(
            &page,
            &format!("action=create-folder&title=Async&parent={id}"),
        );

        assert_eq!(second["tree"][0]["title"], "Rust");
        assert_eq!(second["tree"][0]["children"][0]["title"], "Async");
        assert_eq!(second["tree"][0]["url"], Value::Null);
    }

    #[test]
    fn a_bookmark_is_updated_moved_and_deleted() {
        let service = service();
        add_page(&service);
        let page = page(&service);

        let updated = response(
            &page,
            "action=update&id=1&title=New%20name&url=https%3A%2F%2Fexample.com%2F",
        );
        assert_eq!(updated["tree"][0]["title"], "New name");
        assert_eq!(updated["tree"][0]["url"], "https://example.com/");

        let folder = response(&page, "action=create-folder&title=Folder");
        let id = folder["tree"][1]["id"].as_i64().unwrap();
        let moved = response(&page, &format!("action=move&id=1&parent={id}"));
        assert_eq!(moved["tree"][0]["children"][0]["id"], 1);

        let out = response(&page, "action=move&id=1&parent=&index=0");
        assert_eq!(out["tree"][0]["id"], 1);

        let deleted = response(&page, "action=delete&id=1");
        assert_eq!(deleted["tree"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn a_refused_change_returns_its_message() {
        let service = service();
        let page = page(&service);

        assert_eq!(
            response(&page, "action=create-folder&title=%20"),
            json!({ "error": "A folder needs a name." })
        );
    }

    #[test]
    fn only_web_pages_and_local_files_can_be_saved_as_addresses() {
        let service = service();
        add_page(&service);
        let page = page(&service);

        for address in ["javascript:alert(1)", "about:config", "not an address", ""] {
            let query = url::form_urlencoded::Serializer::new(String::new())
                .append_pair("action", "update")
                .append_pair("id", "1")
                .append_pair("title", "Name")
                .append_pair("url", address)
                .finish();
            assert_eq!(
                response(&page, &query),
                json!({ "error": "Enter a complete address, such as https://example.com/." }),
                "{address}"
            );
        }
    }

    #[test]
    fn malformed_requests_are_refused() {
        let service = service();
        let page = page(&service);

        for query in [
            "action=unknown",
            "action=delete",
            "action=delete&id=abc",
            "action=move&id=1&index=-1",
            "action=move&id=1&parent=abc",
        ] {
            assert_eq!(
                response(&page, query),
                json!({ "error": "This request is not valid." }),
                "{query}"
            );
        }
    }
}
