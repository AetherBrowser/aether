/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Blocks HTTP requests that match a list of sites and URLs.
//!
//! The profile directory holds two files:
//! - `blocklist.txt` — one site, URL, or network filter per line
//! - `content_filter.json` for tracking protection settings:
//!   - `privacy.trackingprotection.enabled`
//!   - `urlclassifier.trackingWhitelistTable` — hosts that are never blocked
//!   - `urlclassifier.trackingTable` — extra filter files, beside `blocklist.txt`
//!
//! `servo:settings#network` writes those files. The resource thread reads them
//! when a request is about to be sent, and again when either file changes.
//!
//! Accepted lines are hosts (`example.com`), URLs (`https://example.com/ads`),
//! and the network rules used by the earlier adblock prototype (`||tracker.com^`,
//! `@@||tracker.com/ok^`, `$script`, `$third-party`, `$domain=`). Cosmetic
//! rules (`##`) are skipped: this filter only refuses network requests.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use log::{debug, warn};
use net_traits::pub_domains::reg_suffix;
use net_traits::request::Destination;
use url::Url;

/// User block list, next to `prefs.json` in the profile directory.
pub const BLOCKLIST_FILE: &str = "blocklist.txt";
/// Enable flag, allow-list, and extra filter files.
pub const CONFIG_FILE: &str = "content_filter.json";

/// `privacy.trackingprotection.enabled` in Firefox.
const PREF_ENABLED: &str = "privacy.trackingprotection.enabled";
/// Firefox stores entity allow-list table names here. Aether stores hosts.
const PREF_WHITELIST: &str = "urlclassifier.trackingWhitelistTable";
/// Firefox stores tracking block-list table names here. Aether stores filter files.
const PREF_FILTER_LISTS: &str = "urlclassifier.trackingTable";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ResourceKind {
    Script,
    Image,
    Stylesheet,
    Font,
    Document,
    Subdocument,
    Object,
    Media,
    Xhr,
    Websocket,
    Ping,
    Other,
}

#[derive(Clone, Debug, Default)]
struct TypeFilter {
    only: Vec<ResourceKind>,
    except: Vec<ResourceKind>,
}

impl TypeFilter {
    fn allows(&self, kind: ResourceKind) -> bool {
        if !self.only.is_empty() && !self.only.contains(&kind) {
            return false;
        }
        !self.except.contains(&kind)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Party {
    Any,
    Third,
    First,
}

#[derive(Clone, Debug)]
struct DomainCond {
    host: String,
    negated: bool,
}

#[derive(Clone, Debug)]
enum Pattern {
    /// A host and its subdomains, on any scheme.
    Site { host: String, port: Option<u16> },
    /// A host plus a path. `exact` refuses a longer path.
    Url {
        host: String,
        port: Option<u16>,
        scheme: Option<String>,
        path_prefix: String,
        exact: bool,
    },
    /// Plain text that must appear in the path or query. Not a regular expression.
    Contains(String),
    /// `*` wildcard matched against `host + path`.
    Glob(String),
}

#[derive(Clone, Debug)]
struct Rule {
    pattern: Pattern,
    exception: bool,
    important: bool,
    types: TypeFilter,
    party: Party,
    domains: Vec<DomainCond>,
}

#[derive(Clone, Debug)]
struct WatchedFile {
    path: PathBuf,
    modified: Option<SystemTime>,
}

struct ParsedRequest {
    scheme: String,
    host: String,
    port: Option<u16>,
    path_and_query: String,
    host_and_path: String,
}

/// Rules loaded from the profile. Safe to share between the public and private
/// HTTP states: both honor the same list.
#[derive(Clone, Debug)]
pub struct ContentFilter {
    enabled: bool,
    whitelist: Vec<String>,
    rules: Vec<Rule>,
    watched: Vec<WatchedFile>,
    config_dir: Option<PathBuf>,
}

impl ContentFilter {
    /// A filter that blocks nothing and does not watch the filesystem.
    pub fn empty() -> Self {
        Self {
            enabled: true,
            whitelist: Vec::new(),
            rules: Vec::new(),
            watched: Vec::new(),
            config_dir: None,
        }
    }

    /// Build a filter from list text. Used by tests and by callers that already
    /// hold the rules in memory.
    pub fn from_list(list: &str) -> Self {
        let (rules, _) = parse_list(list);
        Self {
            enabled: true,
            whitelist: Vec::new(),
            rules,
            watched: Vec::new(),
            config_dir: None,
        }
    }

    /// Load `blocklist.txt`, `content_filter.json`, and any extra lists named there.
    pub fn load(config_dir: Option<&Path>) -> Self {
        let Some(config_dir) = config_dir else {
            return Self::empty();
        };
        let mut filter = Self::empty();
        filter.config_dir = Some(config_dir.to_path_buf());
        filter.reload();
        filter
    }

    /// Re-read the profile files.
    pub fn reload(&mut self) {
        let Some(config_dir) = self.config_dir.clone() else {
            return;
        };
        let config_path = config_dir.join(CONFIG_FILE);
        let blocklist_path = config_dir.join(BLOCKLIST_FILE);
        // Stamp the files before reading them. A save that lands between the
        // read and the stamp would otherwise look fresh while the rules are old.
        let mut watched = vec![watch(&config_path), watch(&blocklist_path)];
        let (enabled, whitelist, lists) = read_config(&config_path);
        let mut texts = Vec::new();
        if let Ok(text) = fs::read_to_string(&blocklist_path) {
            texts.push(text);
        }
        for list in lists {
            let path = if list.is_absolute() {
                list
            } else {
                config_dir.join(list)
            };
            watched.push(watch(&path));
            match fs::read_to_string(&path) {
                Ok(text) => texts.push(text),
                Err(error) => {
                    warn!("Network filter: could not read {}: {error}", path.display());
                },
            }
        }

        let mut rules = Vec::new();
        let mut skipped = 0;
        for text in &texts {
            let (parsed, skipped_here) = parse_list(text);
            rules.extend(parsed);
            skipped += skipped_here;
        }
        if skipped > 0 {
            debug!("Network filter: skipped {skipped} unsupported rules");
        }
        debug!("Network filter: enabled={enabled} rules={}", rules.len());

        self.enabled = enabled;
        self.whitelist = whitelist
            .into_iter()
            .map(|host| normalize_host(&host))
            .collect();
        self.rules = rules;
        self.watched = watched;
    }

    /// Reload when a watched file was created, removed, or written.
    pub fn reload_if_stale(&mut self) {
        if self.is_stale() {
            self.reload();
        }
    }

    pub(crate) fn is_stale(&self) -> bool {
        self.watched
            .iter()
            .any(|file| file.modified != mtime(&file.path))
    }

    /// `true` when this request must not be sent.
    pub fn blocks(&self, url: &str, source: Option<&str>, destination: Destination) -> bool {
        if !self.enabled || self.rules.is_empty() {
            return false;
        }
        let Some(request) = ParsedRequest::parse(url) else {
            return false;
        };
        if self
            .whitelist
            .iter()
            .any(|entry| host_matches(&request.host, entry))
        {
            return false;
        }

        let source_host = source.and_then(host_of);
        let kind = destination_kind(destination);
        let mut blocked = false;
        let mut important_block = false;
        let mut exception = false;
        let mut important_exception = false;
        for rule in &self.rules {
            if !rule.matches(&request, source_host.as_deref(), kind) {
                continue;
            }
            if rule.exception {
                exception = true;
                important_exception |= rule.important;
            } else {
                blocked = true;
                important_block |= rule.important;
            }
        }
        if important_block && !important_exception {
            return true;
        }
        if exception {
            return false;
        }
        blocked
    }
}

impl Default for ContentFilter {
    fn default() -> Self {
        Self::empty()
    }
}

impl ParsedRequest {
    fn parse(url: &str) -> Option<Self> {
        let parsed = Url::parse(url).ok()?;
        let scheme = parsed.scheme().to_ascii_lowercase();
        if !matches!(scheme.as_str(), "http" | "https" | "ws" | "wss") {
            return None;
        }
        let host = normalize_host(parsed.host_str()?);
        if host.is_empty() {
            return None;
        }
        let port = parsed.port();
        let mut path_and_query = parsed.path().to_owned();
        if let Some(query) = parsed.query() {
            path_and_query.push('?');
            path_and_query.push_str(query);
        }
        let mut host_and_path = host.clone();
        host_and_path.push_str(&path_and_query);
        Some(Self {
            scheme,
            host,
            port,
            path_and_query,
            host_and_path,
        })
    }
}

impl Rule {
    fn matches(
        &self,
        request: &ParsedRequest,
        source_host: Option<&str>,
        kind: ResourceKind,
    ) -> bool {
        if !self.types.allows(kind) {
            return false;
        }
        let third_party = source_host.is_some_and(|source| !same_site(&request.host, source));
        match self.party {
            Party::Any => {},
            Party::Third if !third_party => return false,
            Party::First if !source_host.is_some_and(|source| same_site(&request.host, source)) => {
                return false;
            },
            Party::Third | Party::First => {},
        }
        if !source_domains_allow(&self.domains, source_host) {
            return false;
        }
        pattern_matches(&self.pattern, request)
    }
}

fn destination_kind(destination: Destination) -> ResourceKind {
    match destination {
        Destination::Script |
        Destination::Worker |
        Destination::SharedWorker |
        Destination::ServiceWorker => ResourceKind::Script,
        Destination::Image => ResourceKind::Image,
        Destination::Style => ResourceKind::Stylesheet,
        Destination::Font => ResourceKind::Font,
        Destination::Document => ResourceKind::Document,
        Destination::Frame | Destination::IFrame | Destination::Embed => ResourceKind::Subdocument,
        Destination::Object => ResourceKind::Object,
        Destination::Audio | Destination::Video | Destination::Track => ResourceKind::Media,
        Destination::Json | Destination::None | Destination::Text => ResourceKind::Xhr,
        Destination::AudioWorklet |
        Destination::PaintWorklet |
        Destination::Manifest |
        Destination::Report |
        Destination::WebIdentity |
        Destination::Xslt => ResourceKind::Other,
    }
}

fn parse_list(text: &str) -> (Vec<Rule>, usize) {
    let mut rules = Vec::new();
    let mut skipped = 0;
    for line in text.lines() {
        let line = line.trim().trim_start_matches('\u{feff}');
        if line.is_empty() ||
            line.starts_with('!') ||
            line.starts_with('#') ||
            line.starts_with('[')
        {
            continue;
        }
        if line.contains("##") || line.contains("#@#") || line.contains("#?#") {
            continue;
        }
        match parse_rule(line) {
            Some(rule) => rules.push(rule),
            None => skipped += 1,
        }
    }
    (rules, skipped)
}

fn parse_rule(line: &str) -> Option<Rule> {
    let (line, exception) = match line.strip_prefix("@@") {
        Some(rest) => (rest.trim(), true),
        None => (line, false),
    };
    let (pattern_text, options) = split_options(line);
    let pattern_text = pattern_text.trim();
    if pattern_text.is_empty() {
        return None;
    }
    let (types, party, domains, important) = match options {
        Some(options) => parse_options(options)?,
        None => (TypeFilter::default(), Party::Any, Vec::new(), false),
    };
    Some(Rule {
        pattern: parse_pattern(pattern_text)?,
        exception,
        important,
        types,
        party,
        domains,
    })
}

fn split_options(line: &str) -> (&str, Option<&str>) {
    let Some(index) = line.rfind('$') else {
        return (line, None);
    };
    let options = &line[index + 1..];
    if options.is_empty() || !options.split(',').all(option_is_shaped) {
        return (line, None);
    }
    (&line[..index], Some(options))
}

fn option_is_shaped(token: &str) -> bool {
    let token = token.trim();
    if token.is_empty() {
        return false;
    }
    let (name, value) = token.split_once('=').unwrap_or((token, ""));
    if name.is_empty() ||
        !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '~' || c == '-')
    {
        return false;
    }
    value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '~' | '-' | '.' | '|'))
}

fn parse_options(options: &str) -> Option<(TypeFilter, Party, Vec<DomainCond>, bool)> {
    let mut types = TypeFilter::default();
    let mut party = Party::Any;
    let mut domains = Vec::new();
    let mut important = false;
    for token in options.split(',') {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        if token == "important" {
            important = true;
            continue;
        }
        if token == "match-case" {
            continue;
        }
        if token == "third-party" || token == "3p" {
            party = Party::Third;
            continue;
        }
        if token == "~third-party" || token == "first-party" || token == "1p" {
            party = Party::First;
            continue;
        }
        if let Some(list) = token.strip_prefix("domain=") {
            for part in list.split('|') {
                if part.is_empty() {
                    return None;
                }
                if let Some(host) = part.strip_prefix('~') {
                    if host.is_empty() {
                        return None;
                    }
                    domains.push(DomainCond {
                        host: normalize_host(host),
                        negated: true,
                    });
                } else {
                    domains.push(DomainCond {
                        host: normalize_host(part),
                        negated: false,
                    });
                }
            }
            continue;
        }
        if token.contains('=') {
            return None;
        }
        let (negated, name) = match token.strip_prefix('~') {
            Some(name) => (true, name),
            None => (false, token),
        };
        let kind = resource_kind_from_option(name)?;
        if negated {
            types.except.push(kind);
        } else {
            types.only.push(kind);
        }
    }
    Some((types, party, domains, important))
}

fn resource_kind_from_option(name: &str) -> Option<ResourceKind> {
    Some(match name {
        "script" => ResourceKind::Script,
        "image" => ResourceKind::Image,
        "stylesheet" | "css" => ResourceKind::Stylesheet,
        "font" => ResourceKind::Font,
        "document" | "doc" => ResourceKind::Document,
        "subdocument" | "frame" | "iframe" => ResourceKind::Subdocument,
        "object" | "embed" => ResourceKind::Object,
        "media" => ResourceKind::Media,
        "xmlhttprequest" | "xhr" | "fetch" => ResourceKind::Xhr,
        "websocket" => ResourceKind::Websocket,
        "ping" => ResourceKind::Ping,
        "other" => ResourceKind::Other,
        _ => return None,
    })
}

fn parse_pattern(text: &str) -> Option<Pattern> {
    let text = text.trim();
    if text.is_empty() || text == "*" || text == "^" {
        return None;
    }
    if let Some(inner) = text
        .strip_prefix('/')
        .and_then(|rest| rest.strip_suffix('/'))
    {
        if inner.is_empty() || looks_like_regular_expression(inner) {
            return None;
        }
        return Some(Pattern::Contains(inner.to_owned()));
    }
    if text.contains('*') {
        let body = text
            .trim_start_matches('|')
            .trim_end_matches('^')
            .trim()
            .to_ascii_lowercase();
        if body.is_empty() || body.chars().all(|character| character == '*') {
            return None;
        }
        return Some(Pattern::Glob(body));
    }
    if let Some(exact) = text
        .strip_prefix('|')
        .filter(|rest| !rest.starts_with('|'))
        .and_then(|rest| rest.strip_suffix('|'))
    {
        return loose_pattern(exact.trim(), true);
    }
    if let Some(host_rule) = text.strip_prefix("||") {
        return loose_pattern(host_rule.trim_end_matches('^').trim(), false);
    }
    if let Some(anchored) = text.strip_prefix('|') {
        return loose_pattern(anchored.trim_end_matches('^').trim(), false);
    }
    loose_pattern(text.trim_end_matches('^').trim(), false)
}

fn looks_like_regular_expression(text: &str) -> bool {
    text.chars().any(|character| {
        matches!(
            character,
            '\\' | '(' | ')' | '[' | ']' | '{' | '}' | '+' | '?' | '|' | '*' | '^' | '$'
        )
    })
}

fn loose_pattern(text: &str, exact: bool) -> Option<Pattern> {
    if text.is_empty() {
        return None;
    }
    let with_scheme = text.contains("://");
    let parsed = if with_scheme {
        Url::parse(text).ok()?
    } else {
        Url::parse(&format!("http://{text}")).ok()?
    };
    let host = normalize_host(parsed.host_str()?);
    if host.is_empty() || host == "*" {
        return None;
    }
    let scheme = with_scheme.then(|| parsed.scheme().to_ascii_lowercase());
    let explicit_path = parsed.query().is_some() || parsed.path() != "/" || text.ends_with('/');
    if !explicit_path {
        return Some(Pattern::Site {
            host,
            port: parsed.port(),
        });
    }
    let mut path_prefix = parsed.path().to_owned();
    if let Some(query) = parsed.query() {
        path_prefix.push('?');
        path_prefix.push_str(query);
    }
    Some(Pattern::Url {
        host,
        port: parsed.port(),
        scheme,
        path_prefix,
        exact,
    })
}

fn pattern_matches(pattern: &Pattern, request: &ParsedRequest) -> bool {
    match pattern {
        Pattern::Site { host, port } => {
            host_matches(&request.host, host) && port_matches(*port, &request.scheme, request.port)
        },
        Pattern::Url {
            host,
            port,
            scheme,
            path_prefix,
            exact,
        } => {
            if !host_matches(&request.host, host) ||
                !port_matches(*port, &request.scheme, request.port)
            {
                return false;
            }
            if let Some(scheme) = scheme &&
                scheme != &request.scheme
            {
                return false;
            }
            path_matches(&request.path_and_query, path_prefix, *exact)
        },
        Pattern::Contains(text) => request.path_and_query.contains(text.as_str()),
        Pattern::Glob(pattern) => {
            let target = if pattern.contains('/') {
                request.host_and_path.as_str()
            } else {
                request.host.as_str()
            };
            glob_matches_site(pattern, target)
        },
    }
}

fn path_matches(path: &str, prefix: &str, exact: bool) -> bool {
    if exact || path == prefix {
        return path == prefix;
    }
    path.starts_with(prefix) &&
        (prefix.ends_with('/') ||
            matches!(path.as_bytes().get(prefix.len()), Some(b'/') | Some(b'?')))
}

fn port_matches(rule_port: Option<u16>, scheme: &str, request_port: Option<u16>) -> bool {
    let Some(rule_port) = rule_port else {
        return true;
    };
    effective_port(scheme, request_port) == Some(rule_port)
}

fn effective_port(scheme: &str, port: Option<u16>) -> Option<u16> {
    port.or(match scheme {
        "http" | "ws" => Some(80),
        "https" | "wss" => Some(443),
        _ => None,
    })
}

fn glob_matches_site(pattern: &str, host_and_path: &str) -> bool {
    let host_and_path = host_and_path.to_ascii_lowercase();
    let mut rest = host_and_path.as_str();
    loop {
        if glob_match(pattern, rest) {
            return true;
        }
        let Some((_, after_dot)) = rest.split_once('.') else {
            return false;
        };
        if after_dot.is_empty() {
            return false;
        }
        rest = after_dot;
    }
}

fn glob_match(pattern: &str, text: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == text;
    }
    if !text.starts_with(parts[0]) {
        return false;
    }
    let mut rest = &text[parts[0].len()..];
    for (index, part) in parts.iter().enumerate().skip(1) {
        if index + 1 == parts.len() {
            return rest.ends_with(part);
        }
        if part.is_empty() {
            continue;
        }
        let Some(found) = rest.find(part) else {
            return false;
        };
        rest = &rest[found + part.len()..];
    }
    true
}

fn host_matches(host: &str, rule_host: &str) -> bool {
    let host = host.trim_end_matches('.');
    let rule_host = rule_host.trim_end_matches('.');
    if host.eq_ignore_ascii_case(rule_host) {
        return true;
    }
    host.len() > rule_host.len() &&
        host.as_bytes().get(host.len() - rule_host.len() - 1) == Some(&b'.') &&
        host[host.len() - rule_host.len()..].eq_ignore_ascii_case(rule_host)
}

fn same_site(request_host: &str, source_host: &str) -> bool {
    if host_matches(request_host, source_host) || host_matches(source_host, request_host) {
        return true;
    }
    let request_site = reg_suffix(request_host);
    let source_site = reg_suffix(source_host);
    !request_site.is_empty() && request_site.eq_ignore_ascii_case(source_site)
}

fn source_domains_allow(domains: &[DomainCond], source_host: Option<&str>) -> bool {
    if domains.is_empty() {
        return true;
    }
    let Some(source_host) = source_host else {
        return false;
    };
    let mut saw_positive = false;
    let mut positive_match = false;
    for domain in domains {
        if domain.negated {
            if host_matches(source_host, &domain.host) {
                return false;
            }
        } else {
            saw_positive = true;
            positive_match |= host_matches(source_host, &domain.host);
        }
    }
    !saw_positive || positive_match
}

fn host_of(url: &str) -> Option<String> {
    Url::parse(url).ok()?.host_str().map(normalize_host)
}

fn normalize_host(host: &str) -> String {
    host.trim_end_matches('.').to_ascii_lowercase()
}

fn read_config(path: &Path) -> (bool, Vec<String>, Vec<PathBuf>) {
    let Ok(text) = fs::read_to_string(path) else {
        return (true, Vec::new(), Vec::new());
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        warn!(
            "Network filter: ignoring unreadable config {}",
            path.display()
        );
        return (true, Vec::new(), Vec::new());
    };
    let enabled = value
        .get(PREF_ENABLED)
        .or_else(|| value.get("enabled"))
        .and_then(|item| item.as_bool())
        .unwrap_or(true);
    let whitelist = string_list(
        value
            .get(PREF_WHITELIST)
            .or_else(|| value.get("whitelist_domains")),
    );
    let lists = string_list(
        value
            .get(PREF_FILTER_LISTS)
            .or_else(|| value.get("filter_lists")),
    )
    .into_iter()
    .map(PathBuf::from)
    .collect();
    (enabled, whitelist, lists)
}

fn string_list(value: Option<&serde_json::Value>) -> Vec<String> {
    value
        .and_then(|item| item.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn watch(path: &Path) -> WatchedFile {
    WatchedFile {
        path: path.to_path_buf(),
        modified: mtime(path),
    }
}

fn mtime(path: &Path) -> Option<SystemTime> {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
}
