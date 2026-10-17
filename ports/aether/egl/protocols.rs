/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! `servo:` and `resource:` pages used by the Android shell.
//!
//! The desktop shell serves these from the resources directory. Android has no
//! such directory in the APK, so the new-tab page and the files it loads are
//! embedded here.

use std::future::Future;
use std::pin::Pin;

use http::HeaderValue;
use http::header::CONTENT_TYPE;
use servo::protocol_handler::{
    DoneChannel, FetchContext, NetworkError, Origin, ProtocolHandler, Referrer, Request,
    ResourceFetchTiming, Response, ResponseBody,
};

const NEWTAB_HTML: &str = include_str!("../../../resources/resource_protocol/newtab.html");
const NEWTAB_CSS: &str = include_str!("../../../resources/resource_protocol/newtab.css");
const LOGO_LIGHT: &[u8] =
    include_bytes!("../../../resources/resource_protocol/servo-color-positive-no-container.svg");
const LOGO_DARK: &[u8] =
    include_bytes!("../../../resources/resource_protocol/servo-color-negative-no-container.svg");

#[derive(Default)]
pub(crate) struct ServoProtocolHandler;

impl ProtocolHandler for ServoProtocolHandler {
    fn is_fetchable(&self) -> bool {
        true
    }

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
            "servo-color-positive-no-container.svg" => {
                bytes_response(request, LOGO_LIGHT, "image/svg+xml")
            },
            "servo-color-negative-no-container.svg" => {
                bytes_response(request, LOGO_DARK, "image/svg+xml")
            },
            _ => error_response("Invalid path"),
        }
    }
}

fn bytes_response(
    request: &Request,
    body: &[u8],
    content_type: &'static str,
) -> Pin<Box<dyn Future<Output = Response> + Send>> {
    let mut response = Response::new(
        request.current_url(),
        ResourceFetchTiming::new(request.timing_type()),
    );
    if let Ok(value) = HeaderValue::from_str(content_type) {
        response.headers.insert(CONTENT_TYPE, value);
    }
    *response.body.lock() = ResponseBody::Done(body.to_vec());
    Box::pin(std::future::ready(response))
}

fn error_response(message: &str) -> Pin<Box<dyn Future<Output = Response> + Send>> {
    Box::pin(std::future::ready(Response::network_error(
        NetworkError::ResourceLoadError(message.to_owned()),
    )))
}

fn forbidden_response() -> Pin<Box<dyn Future<Output = Response> + Send>> {
    error_response("Forbidden")
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
