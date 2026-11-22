/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! `about:` and `resource:` pages used by the Android shell.
//!
//! The desktop shell serves these from the resources directory. Android has no
//! such directory in the APK, so its pages and the files they load are embedded
//! here.

use std::future::Future;
use std::pin::Pin;

use http::header::CONTENT_TYPE;
use http::{HeaderName, HeaderValue};
use servo::protocol_handler::{
    DoneChannel, FetchContext, NetworkError, Origin, ProtocolHandler, Referrer, Request,
    ResourceFetchTiming, Response, ResponseBody,
};

use crate::bookmarks_page::BookmarksPage;

const NEWTAB_HTML: &str = include_str!("../../../resources/resource_protocol/newtab.html");
const BOOKMARKS_HTML: &str = include_str!("../../../resources/resource_protocol/bookmarks.html");
const NEWTAB_CSS: &str = include_str!("../../../resources/resource_protocol/newtab.css");
const LOGO_LIGHT: &[u8] =
    include_bytes!("../../../resources/resource_protocol/aether-color-positive-no-container.svg");
const LOGO_DARK: &[u8] =
    include_bytes!("../../../resources/resource_protocol/aether-color-negative-no-container.svg");
/// The shared style sheets of the bookmarks page.
const STYLE_SHEETS: [(&str, &str); 6] = [
    (
        "style/page.css",
        include_str!("../../../resources/resource_protocol/style/page.css"),
    ),
    (
        "style/button.css",
        include_str!("../../../resources/resource_protocol/style/button.css"),
    ),
    (
        "style/menu.css",
        include_str!("../../../resources/resource_protocol/style/menu.css"),
    ),
    (
        "style/input.css",
        include_str!("../../../resources/resource_protocol/style/input.css"),
    ),
    (
        "style/enum_list.css",
        include_str!("../../../resources/resource_protocol/style/enum_list.css"),
    ),
    (
        "style/card.css",
        include_str!("../../../resources/resource_protocol/style/card.css"),
    ),
];

pub(crate) struct ServoProtocolHandler {
    bookmarks_page: BookmarksPage,
}

impl ServoProtocolHandler {
    pub(crate) fn new(bookmarks_page: BookmarksPage) -> Self {
        Self { bookmarks_page }
    }
}

impl ProtocolHandler for ServoProtocolHandler {
    fn load(
        &self,
        request: &mut Request,
        _done_chan: &mut DoneChannel,
        _context: &FetchContext,
    ) -> Pin<Box<dyn Future<Output = Response> + Send>> {
        let url = request.current_url();
        match url.path().trim_start_matches('/') {
            "newtab" => bytes_response(request, NEWTAB_HTML.as_bytes(), "text/html; charset=utf-8"),
            // `newtab.html` reads this with an empty query. A navigation or a
            // page from the web must not see or change the homepage settings.
            "home"
                if !request.is_navigation_request() &&
                    !request_is_from_web_content(request) &&
                    url.query().is_none_or(str::is_empty) =>
            {
                bytes_response(
                    request,
                    crate::home::handle_home_request(None).as_bytes(),
                    "application/json",
                )
            },
            "home" => forbidden_response(),
            "bookmarks" => page_response(request, BOOKMARKS_HTML.as_bytes()),
            "bookmarks-data"
                if !request.is_navigation_request() &&
                    !request_is_from_web_content(request) &&
                    request_is_from_bookmarks_page(request) =>
            {
                json_response_when_ready(request, self.bookmarks_page.handle_request(url.query()))
            },
            "bookmarks-data" => forbidden_response(),
            // Read-only here. The desktop settings page is what writes the list.
            "content-filter"
                if !request.is_navigation_request() &&
                    !request_is_from_web_content(request) &&
                    url.query().is_none_or(str::is_empty) =>
            {
                bytes_response(
                    request,
                    crate::content_filter::handle_content_filter_request(None).as_bytes(),
                    "application/json",
                )
            },
            "content-filter" => forbidden_response(),
            _ => error_response("Invalid shortcut"),
        }
    }
}

#[derive(Default)]
pub(crate) struct ResourceProtocolHandler;

impl ProtocolHandler for ResourceProtocolHandler {
    fn load(
        &self,
        request: &mut Request,
        _done_chan: &mut DoneChannel,
        _context: &FetchContext,
    ) -> Pin<Box<dyn Future<Output = Response> + Send>> {
        let url = request.current_url();
        match url.path().trim_start_matches('/') {
            "newtab.css" => {
                bytes_response(request, NEWTAB_CSS.as_bytes(), "text/css; charset=utf-8")
            },
            "aether-color-positive-no-container.svg" => {
                bytes_response(request, LOGO_LIGHT, "image/svg+xml")
            },
            "aether-color-negative-no-container.svg" => {
                bytes_response(request, LOGO_DARK, "image/svg+xml")
            },
            path => match STYLE_SHEETS.iter().find(|(name, _)| *name == path) {
                Some((_, css)) => {
                    bytes_response(request, css.as_bytes(), "text/css; charset=utf-8")
                },
                None => error_response("Invalid path"),
            },
        }
    }
}

fn bytes_response(
    request: &Request,
    body: &[u8],
    content_type: &'static str,
) -> Pin<Box<dyn Future<Output = Response> + Send>> {
    Box::pin(std::future::ready(response_with_body(
        request,
        body,
        content_type,
    )))
}

/// Loads an internal page, which web pages cannot show in a frame.
fn page_response(request: &Request, body: &[u8]) -> Pin<Box<dyn Future<Output = Response> + Send>> {
    let mut response = response_with_body(request, body, "text/html; charset=utf-8");
    response.headers.insert(
        HeaderName::from_static("x-frame-options"),
        HeaderValue::from_static("DENY"),
    );
    Box::pin(std::future::ready(response))
}

fn json_response_when_ready(
    request: &Request,
    body: impl Future<Output = String> + Send + 'static,
) -> Pin<Box<dyn Future<Output = Response> + Send>> {
    let response = response_with_body(request, &[], "application/json");
    Box::pin(async move {
        *response.body.lock() = ResponseBody::Done(body.await.into_bytes());
        response
    })
}

fn response_with_body(request: &Request, body: &[u8], content_type: &'static str) -> Response {
    let mut response = Response::new(
        request.current_url(),
        ResourceFetchTiming::new(request.timing_type()),
    );
    if let Ok(value) = HeaderValue::from_str(content_type) {
        response.headers.insert(CONTENT_TYPE, value);
    }
    *response.body.lock() = ResponseBody::Done(body.to_vec());
    response
}

fn error_response(message: &str) -> Pin<Box<dyn Future<Output = Response> + Send>> {
    Box::pin(std::future::ready(Response::network_error(
        NetworkError::ResourceLoadError(message.to_owned()),
    )))
}

fn forbidden_response() -> Pin<Box<dyn Future<Output = Response> + Send>> {
    error_response("Forbidden")
}

fn request_is_from_bookmarks_page(request: &Request) -> bool {
    request.source_document_url.as_ref().is_some_and(|url| {
        let url = url.as_url();
        url.scheme() == "about" && url.path() == "bookmarks"
    })
}

fn request_is_from_web_content(request: &Request) -> bool {
    fn is_web_scheme(scheme: &str) -> bool {
        matches!(scheme, "http" | "https" | "ftp" | "ws" | "wss")
    }

    if let Origin::Origin(origin) = &request.origin &&
        origin.scheme().is_some_and(is_web_scheme)
    {
        return true;
    }
    match &request.referrer {
        Referrer::Client(url) | Referrer::ReferrerUrl(url) => is_web_scheme(url.scheme()),
        Referrer::NoReferrer => false,
    }
}
