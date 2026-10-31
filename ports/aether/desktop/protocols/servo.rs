/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Loads resources using a mapping from well-known shortcuts to resource: urls.
//! Recognized shortcuts:
//! - about:default-user-agent
//! - about:experimental-preferences
//! - about:config
//! - about:newtab
//! - about:preferences
//! - about:processes
//! - about:process-list
//! - about:history
//! - about:settings
//! - about:home
//! - about:about
//! - about:search-engines
//! - about:history-data

use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

use headers::{ContentType, HeaderMapExt, HeaderName, HeaderValue};
use servo::UserAgentPlatform;
use servo::protocol_handler::{
    DoneChannel, FetchContext, NetworkError, Origin, ProtocolHandler, Referrer, Request,
    ResourceFetchTiming, Response, ResponseBody,
};

use crate::desktop::protocols::processes::ProcessSampler;
use crate::desktop::protocols::resource::ResourceProtocolHandler;
use crate::history::HistoryPage;
use crate::prefs::EXPERIMENTAL_PREFS;

pub struct ServoProtocolHandler {
    process_sampler: Mutex<ProcessSampler>,
    history_page: Mutex<HistoryPage>,
}

impl ServoProtocolHandler {
    pub fn new(history_page: HistoryPage) -> Self {
        Self {
            process_sampler: Mutex::new(ProcessSampler::default()),
            history_page: Mutex::new(history_page),
        }
    }
}

impl ProtocolHandler for ServoProtocolHandler {
    fn privileged_paths(&self) -> &'static [&'static str] {
        &["config", "preferences"]
    }

    fn is_fetchable(&self) -> bool {
        true
    }

    fn load(
        &self,
        request: &mut Request,
        done_chan: &mut DoneChannel,
        context: &FetchContext,
    ) -> Pin<Box<dyn Future<Output = Response> + Send>> {
        let url = request.current_url();

        match url.path() {
            "config" => page_response(request, done_chan, context, "/config.html"),
            "newtab" => page_response(request, done_chan, context, "/newtab.html"),

            "preferences" => page_response(request, done_chan, context, "/preferences.html"),

            "license" => page_response(request, done_chan, context, "/license.html"),

            "processes" => page_response(request, done_chan, context, "/processes.html"),

            "history" => page_response(request, done_chan, context, "/history.html"),

            "settings" => page_response(request, done_chan, context, "/settings.html"),

            // Internal JSON used by `settings.html` and `newtab.html`. A
            // navigation would show that JSON to the user. Web pages can also
            // fetch this scheme, because `is_fetchable` is true. A non-empty
            // query persists settings, so only `about:settings` may send one.
            // `newtab.html` reads with an empty query.
            "home" => {
                if !internal_json_request_is_allowed(request, url.query()) {
                    return forbidden_response();
                }
                json_response(request, crate::home::handle_home_request(url.query()))
            },

            "about" => {
                if request_is_from_web_content(request) {
                    return forbidden_response();
                }
                json_response(request, crate::about_info_json())
            },

            "search-engines" => {
                if !internal_json_request_is_allowed(request, url.query()) {
                    return forbidden_response();
                }
                json_response(
                    request,
                    crate::search::handle_search_engines_request(url.query()),
                )
            },

            "history-data" => {
                if internal_endpoint_is_hidden(request) || !request_is_from_history_page(request) {
                    return forbidden_response();
                }
                let body = match self.history_page.lock() {
                    Ok(mut page) => page.handle_request(url.query()),
                    Err(poisoned) => poisoned.into_inner().handle_request(url.query()),
                };
                json_response_when_ready(request, body)
            },

            "process-list" => {
                if request_is_from_web_content(request) {
                    return Box::pin(std::future::ready(Response::network_error(
                        NetworkError::ResourceLoadError("Forbidden".to_owned()),
                    )));
                }
                let body = match self.process_sampler.lock() {
                    Ok(mut sampler) => sampler.snapshot_json(),
                    Err(poisoned) => poisoned.into_inner().snapshot_json(),
                };
                json_response(request, body)
            },

            "experimental-preferences" => {
                let pref_list = EXPERIMENTAL_PREFS
                    .iter()
                    .map(|pref| format!("\"{pref}\""))
                    .collect::<Vec<String>>()
                    .join(",");
                json_response(request, format!("[{pref_list}]"))
            },

            "default-user-agent" => {
                let user_agent = UserAgentPlatform::default().to_user_agent_string();
                json_response(request, format!("\"{user_agent}\""))
            },

            _ => Box::pin(std::future::ready(Response::network_error(
                NetworkError::ResourceLoadError("Invalid shortcut".to_owned()),
            ))),
        }
    }
}

fn internal_endpoint_is_hidden(request: &Request) -> bool {
    request.is_navigation_request() || request_is_from_web_content(request)
}

/// Reads stay available to internal pages such as `newtab.html`. A query
/// writes settings, and `file:` or another opaque caller is not rejected by
/// [`request_is_from_web_content`].
fn internal_json_request_is_allowed(request: &Request, query: Option<&str>) -> bool {
    if internal_endpoint_is_hidden(request) {
        return false;
    }
    query.is_none_or(|query| query.is_empty()) || request_is_from_settings_page(request)
}

fn request_is_from_settings_page(request: &Request) -> bool {
    request
        .source_document_url
        .as_ref()
        .is_some_and(|url| is_settings_page_url(url.as_url()))
}

fn is_settings_page_url(url: &url::Url) -> bool {
    url.scheme() == "about" && url.path() == "settings"
}

fn request_is_from_history_page(request: &Request) -> bool {
    request
        .source_document_url
        .as_ref()
        .is_some_and(|url| is_history_page_url(url.as_url()))
}

fn is_history_page_url(url: &url::Url) -> bool {
    url.scheme() == "about" && url.path() == "history"
}

/// Loads an internal page, which web pages cannot show in a frame.
fn page_response(
    request: &mut Request,
    done_chan: &mut DoneChannel,
    context: &FetchContext,
    path: &str,
) -> Pin<Box<dyn Future<Output = Response> + Send>> {
    let response = ResourceProtocolHandler::response_for_path(request, done_chan, context, path);
    Box::pin(async move {
        let mut response = response.await;
        response.headers.insert(
            HeaderName::from_static("x-frame-options"),
            HeaderValue::from_static("DENY"),
        );
        response
    })
}

fn forbidden_response() -> Pin<Box<dyn Future<Output = Response> + Send>> {
    Box::pin(std::future::ready(Response::network_error(
        NetworkError::ResourceLoadError("Forbidden".to_owned()),
    )))
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

fn json_response(
    request: &Request,
    body: String,
) -> Pin<Box<dyn Future<Output = Response> + Send>> {
    json_response_when_ready(request, std::future::ready(body))
}

fn json_response_when_ready(
    request: &Request,
    body: impl Future<Output = String> + Send + 'static,
) -> Pin<Box<dyn Future<Output = Response> + Send>> {
    let url = request.current_url();
    let timing_type = request.timing_type();
    Box::pin(async move {
        let mut response = Response::new(url, ResourceFetchTiming::new(timing_type));
        response.headers.typed_insert(ContentType::json());
        *response.body.lock() = ResponseBody::Done(body.await.into_bytes());
        response
    })
}

#[cfg(test)]
mod tests {
    use url::Url;

    use super::{is_history_page_url, is_settings_page_url};

    #[test]
    fn settings_page_url_is_the_settings_path() {
        let settings = Url::parse("about:settings#home").unwrap();
        assert!(is_settings_page_url(&settings));

        let newtab = Url::parse("about:newtab").unwrap();
        assert!(!is_settings_page_url(&newtab));

        let file = Url::parse("file:///tmp/page.html").unwrap();
        assert!(!is_settings_page_url(&file));
    }

    #[test]
    fn history_page_url_is_the_history_path() {
        assert!(is_history_page_url(&Url::parse("about:history").unwrap()));
        assert!(!is_history_page_url(
            &Url::parse("about:history-data").unwrap()
        ));
        assert!(!is_history_page_url(
            &Url::parse("https://example.com/history").unwrap()
        ));
    }
}
