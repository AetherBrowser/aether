/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use http_body_util::combinators::BoxBody;
use hyper::body::{Bytes, Incoming};
use hyper::{Request as HyperRequest, Response as HyperResponse};
use net::content_filter::{BLOCKLIST_FILE, CONFIG_FILE, ContentFilter};
use net_traits::NetworkError;
use net_traits::request::{Destination, Referrer, RequestBuilder};
use servo_base::id::TEST_WEBVIEW_ID;

use crate::{fetch_with_context, make_body, make_server, new_fetch_context};

fn blocks(list: &str, url: &str) -> bool {
    ContentFilter::from_list(list).blocks(url, None, Destination::Document)
}

fn blocks_as(list: &str, url: &str, source: &str, destination: Destination) -> bool {
    ContentFilter::from_list(list).blocks(url, Some(source), destination)
}

#[test]
fn host_blocks_that_site_and_its_subdomains() {
    let list = "ads.example\n";
    assert!(blocks(list, "https://ads.example/banner.jpg"));
    assert!(blocks(list, "http://cdn.ads.example/pixel"));
    assert!(!blocks(list, "https://example/"));
    assert!(!blocks(list, "https://notads.example/"));
    assert!(!blocks(list, "https://ads.example.evil/"));
}

#[test]
fn url_blocks_that_path_only() {
    let list = "https://ads.example/banner\n";
    assert!(blocks(list, "https://ads.example/banner"));
    assert!(blocks(list, "https://ads.example/banner/large.js"));
    assert!(blocks(list, "https://cdn.ads.example/banner"));
    assert!(!blocks(list, "https://ads.example/bannerish"));
    assert!(!blocks(list, "http://ads.example/banner"));
    assert!(!blocks(list, "https://ads.example/other"));
}

#[test]
fn adblock_network_rules_and_exceptions() {
    let list = "\
||tracker.example^
||analytics.example^$script
||pixels.example^$third-party
@@||tracker.example/allowed.js
/pixel.gif/
";
    assert!(blocks(list, "https://tracker.example/a.js"));
    assert!(!blocks(list, "https://tracker.example/allowed.js"));
    assert!(blocks_as(
        list,
        "https://analytics.example/track.js",
        "https://news.example/",
        Destination::Script,
    ));
    assert!(!blocks_as(
        list,
        "https://analytics.example/track.js",
        "https://news.example/",
        Destination::Image,
    ));
    assert!(blocks_as(
        list,
        "https://pixels.example/a",
        "https://news.example/",
        Destination::Image,
    ));
    assert!(!blocks_as(
        list,
        "https://pixels.example/a",
        "https://www.pixels.example/",
        Destination::Image,
    ));
    assert!(blocks(list, "https://news.example/static/pixel.gif"));
    assert!(!blocks(list, "https://news.example/"));
}

#[test]
fn comments_cosmetic_rules_and_unknown_options_are_skipped() {
    let list = "\
! a comment
# another comment
example.com##.ad
||popup.example^$popup
kept.example
";
    assert!(!blocks(list, "https://example.com/"));
    assert!(!blocks(list, "https://popup.example/"));
    assert!(blocks(list, "https://kept.example/"));
}

#[test]
fn wildcard_separator_scheme_case_and_anchors() {
    assert!(blocks("||tracker.example^*\n", "https://tracker.example/a.js"));
    assert!(!blocks(
        "||ads.example/*\n",
        "https://safe.example/redirect.ads.example/banner",
    ));
    assert!(blocks(
        "https://ads.example/banner/*\n",
        "https://ads.example/banner/a.js",
    ));
    assert!(!blocks(
        "https://ads.example/banner/*\n",
        "http://ads.example/banner/a.js",
    ));
    assert!(blocks("https://ads.example\n", "https://ads.example/a"));
    assert!(!blocks("https://ads.example\n", "http://ads.example/a"));
    assert!(blocks(
        "https://ads.example:443/banner\n",
        "https://ads.example/banner",
    ));
    assert!(!blocks(
        "https://ads.example:443/banner\n",
        "https://ads.example:8443/banner",
    ));
    assert!(blocks(
        "|https://ads.example/banner|\n",
        "https://ads.example/banner",
    ));
    assert!(!blocks(
        "|https://ads.example/banner|\n",
        "https://cdn.ads.example/banner",
    ));
    assert!(blocks("||tracker.example/ads\n", "https://tracker.example/ads.js"));
    assert!(!blocks(
        "||ads.example/Banner*$match-case\n",
        "https://ads.example/banner.js",
    ));
    assert!(blocks(
        "||ads.example/Banner*$match-case\n",
        "https://ads.example/Banner.js",
    ));
    assert!(blocks_as(
        "|https://ads.example/banner/*$script\n",
        "https://ads.example/banner/a.js",
        "https://news.example/",
        Destination::Script,
    ));
    assert!(!blocks_as(
        "|https://ads.example/banner/*$script\n",
        "https://ads.example/banner/a.js",
        "https://news.example/",
        Destination::Image,
    ));
    assert!(blocks(
        "|https://ads.example/banner/*|\n",
        "https://ads.example/banner/a.js",
    ));
    assert!(!blocks(
        "|https://ads.example/banner\n",
        "https://cdn.ads.example/banner",
    ));
    assert!(blocks(
        "https://ads.example:8443/banner/*\n",
        "https://ads.example:8443/banner/a.js",
    ));
    assert!(!blocks(
        "https://ads.example:8443/banner/*\n",
        "https://ads.example/banner/a.js",
    ));
    assert!(blocks(
        "*tracking_pixel*\n",
        "https://news.example/assets/tracking_pixel.gif",
    ));
    assert!(blocks(
        "*banner^\n",
        "https://tracker.example/banner?x=1",
    ));
    assert!(blocks(
        "https://ads.example/file$metadata\n",
        "https://ads.example/file$metadata",
    ));
    assert!(blocks_as(
        "||cdn.example^$third-party\n",
        "https://cdn.example/a.js",
        "https://tenant.github.io/",
        Destination::Script,
    ));
    assert!(blocks_as(
        "||cdn.example^$third-party\n",
        "https://cdn.example/a.js",
        "https://other.github.io/",
        Destination::Script,
    ));
    assert!(blocks_as(
        "||widgets.example^$object\n",
        "https://widgets.example/plugin",
        "https://news.example/",
        Destination::Embed,
    ));
    assert!(ContentFilter::from_list("||realtime.example^$websocket\n").blocks_websocket(
        "wss://realtime.example/socket",
        Some("https://news.example/"),
    ));
}

#[test]
fn a_broken_config_keeps_the_previous_rules() {
    let dir = temp_dir("broken");
    fs::write(dir.join(BLOCKLIST_FILE), "blocked.example\n").unwrap();
    fs::write(
        dir.join(CONFIG_FILE),
        r#"{ "privacy.trackingprotection.enabled": true, "urlclassifier.trackingWhitelistTable": ["allowed.example"], "urlclassifier.trackingTable": [] }"#,
    )
    .unwrap();
    let mut filter = ContentFilter::load(Some(&dir));
    assert!(filter.blocks("https://blocked.example/", None, Destination::Document));
    assert!(!filter.blocks("https://allowed.example/", None, Destination::Document));

    fs::write(dir.join(CONFIG_FILE), "{").unwrap();
    filter.reload();
    assert!(filter.blocks("https://blocked.example/", None, Destination::Document));
    assert!(!filter.blocks("https://allowed.example/", None, Destination::Document));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn data_and_file_urls_are_not_filtered() {
    let filter = ContentFilter::from_list("example");
    assert!(!filter.blocks("data:text/plain,hi", None, Destination::Document));
    assert!(!filter.blocks("file:///tmp/example", None, Destination::Document));
}

#[test]
fn profile_files_enable_whitelist_and_extra_lists() {
    let dir = temp_dir("profile");
    fs::write(
        dir.join(BLOCKLIST_FILE),
        "blocked.example\nallowed.example\n",
    )
    .unwrap();
    fs::write(dir.join("extra.txt"), "||extra.example^\n").unwrap();
    fs::write(
        dir.join(CONFIG_FILE),
        r#"{
            "privacy.trackingprotection.enabled": true,
            "urlclassifier.trackingWhitelistTable": ["allowed.example"],
            "urlclassifier.trackingTable": ["extra.txt"]
        }"#,
    )
    .unwrap();

    let filter = ContentFilter::load(Some(&dir));
    assert!(filter.blocks("https://blocked.example/", None, Destination::Document));
    assert!(filter.blocks("https://extra.example/x", None, Destination::Document));
    assert!(!filter.blocks("https://allowed.example/", None, Destination::Document));
    assert!(!filter.blocks("https://www.allowed.example/", None, Destination::Document));

    fs::write(
        dir.join(CONFIG_FILE),
        r#"{ "privacy.trackingprotection.enabled": false, "urlclassifier.trackingWhitelistTable": [], "urlclassifier.trackingTable": ["extra.txt"] }"#,
    )
    .unwrap();
    let mut filter = filter;
    filter.reload_if_stale();
    assert!(!filter.blocks("https://blocked.example/", None, Destination::Document));
    assert!(!filter.blocks("https://extra.example/", None, Destination::Document));

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_new_blocklist_is_picked_up_without_restarting() {
    let dir = temp_dir("reload");
    let mut filter = ContentFilter::load(Some(&dir));
    assert!(!filter.blocks("https://later.example/", None, Destination::Document));

    fs::write(dir.join(BLOCKLIST_FILE), "later.example\n").unwrap();
    filter.reload_if_stale();
    assert!(filter.blocks("https://later.example/", None, Destination::Document));

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn fetch_does_not_send_a_blocked_request() {
    static HITS: AtomicUsize = AtomicUsize::new(0);
    let handler =
        move |_: HyperRequest<Incoming>,
              response: &mut HyperResponse<BoxBody<Bytes, hyper::Error>>| {
            HITS.fetch_add(1, Ordering::SeqCst);
            *response.body_mut() = make_body(b"should not be requested".to_vec());
        };
    let (server, url) = make_server(handler);
    let mut context = new_fetch_context(None, None);
    *context.state.content_filter.write() = ContentFilter::from_list("localhost\n");

    let request = RequestBuilder::new(Some(TEST_WEBVIEW_ID), url.clone(), Referrer::NoReferrer)
        .origin(url.origin())
        .policy_container(Default::default())
        .build();
    let response = fetch_with_context(request, &mut context);
    let _ = server.close();

    assert_eq!(
        response.get_network_error(),
        Some(&NetworkError::ContentBlocked)
    );
    assert_eq!(HITS.load(Ordering::SeqCst), 0);
}

fn temp_dir(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "aether-content-filter-{}-{nanos}-{name}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}
