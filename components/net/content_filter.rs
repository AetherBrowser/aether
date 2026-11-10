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
    /// A host, and its subdomains unless `host_exact`. `scheme` limits the rule
    /// when the list entry included one (`https://ads.example`).
    Site {
        host: String,
        port: Option<u16>,
        scheme: Option<String>,
        host_exact: bool,
    },
    /// A host plus a path. `exact` refuses a longer path. `boundary` requires
    /// the next character to be `/` or `?` (plain URL entries). Network rules
    /// such as `||tracker.example/ads` leave it false so `/ads.js` still matches.
    Url {
        host: String,
        port: Option<u16>,
        scheme: Option<String>,
        path_prefix: String,
        exact: bool,
        host_exact: bool,
        boundary: bool,
    },
    /// Plain text that must appear in the path or query. Not a regular expression.
    Contains(String),
    /// `*` wildcard. `^` is a separator. `case_sensitive` honors `$match-case`.
    /// `domain` is `||` (hostname labels). `start` and `end` are single `|` anchors.
    Glob {
        text: String,
        case_sensitive: bool,
        domain: bool,
        start: bool,
        end: bool,
    },
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
    len: u64,
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
        let (enabled, whitelist, lists) = match read_config(&config_path) {
            Ok(config) => config,
            Err(()) => {
                warn!(
                    "Network filter: keeping the previous rules, {} could not be read",
                    config_path.display()
                );
                if !self.watched.is_empty() {
                    for file in &mut self.watched {
                        let stat = file_stat(&file.path);
                        file.modified = stat.modified;
                        file.len = stat.len;
                    }
                    return;
                }
                (true, Vec::new(), Vec::new())
            },
        };
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
        self.watched.iter().any(|file| {
            let stat = file_stat(&file.path);
            // Size is checked as well as mtime. Some filesystems only store
            // modification times to the second, so a rewrite in that same
            // second would otherwise keep the previous rules.
            stat.modified != file.modified || stat.len != file.len
        })
    }

    /// `true` when this request must not be sent.
    pub fn blocks(&self, url: &str, source: Option<&str>, destination: Destination) -> bool {
        self.decide(url, source, destination_kind(destination))
    }

    /// `$websocket` rules. WebSocket requests do not use a fetch destination.
    pub fn blocks_websocket(&self, url: &str, source: Option<&str>) -> bool {
        self.decide(url, source, ResourceKind::Websocket)
    }

    fn decide(&self, url: &str, source: Option<&str>, kind: ResourceKind) -> bool {
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
        Destination::Frame | Destination::IFrame => ResourceKind::Subdocument,
        Destination::Object | Destination::Embed => ResourceKind::Object,
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
    let parsed_options = options.and_then(parse_options);
    // A trailing `$metadata` or `$script` on a URL is part of the address.
    // Real options (`$script` after `||host^`, `$domain=`) still parse.
    let (pattern_text, parsed_options) = match parsed_options {
        Some(parsed) => (pattern_text, Some(parsed)),
        None => (line, None),
    };
    let pattern_text = pattern_text.trim();
    if pattern_text.is_empty() {
        return None;
    }
    let (types, party, domains, important, case_sensitive) =
        parsed_options.unwrap_or((TypeFilter::default(), Party::Any, Vec::new(), false, false));
    Some(Rule {
        pattern: parse_pattern(pattern_text, case_sensitive)?,
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
    // `https://ads.example/file$script` is a URL. Anchored and wildcard rules
    // still carry options: `|https://ads.example/banner/*$script`.
    let pattern = &line[..index];
    let filter_syntax = pattern.starts_with('|') || pattern.contains('*');
    if pattern.contains("://") && !filter_syntax && !options.contains(',') && !options.contains('=')
    {
        return (line, None);
    }
    (pattern, Some(options))
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

fn parse_options(options: &str) -> Option<(TypeFilter, Party, Vec<DomainCond>, bool, bool)> {
    let mut types = TypeFilter::default();
    let mut party = Party::Any;
    let mut domains = Vec::new();
    let mut important = false;
    let mut case_sensitive = false;
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
            case_sensitive = true;
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
    Some((types, party, domains, important, case_sensitive))
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

fn parse_pattern(text: &str, case_sensitive: bool) -> Option<Pattern> {
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
        let domain = text.starts_with("||");
        let start = text.starts_with('|');
        let end = text.ends_with('|') && !text.ends_with("||");
        let body = text.trim_matches('|').trim();
        if body.is_empty() ||
            body.chars()
                .all(|character| character == '*' || character == '^')
        {
            return None;
        }
        let text = if case_sensitive {
            body.to_owned()
        } else {
            body.to_ascii_lowercase()
        };
        return Some(Pattern::Glob {
            text,
            case_sensitive,
            domain,
            start,
            end,
        });
    }
    if let Some(exact) = text
        .strip_prefix('|')
        .filter(|rest| !rest.starts_with('|'))
        .and_then(|rest| rest.strip_suffix('|'))
    {
        return loose_pattern(exact.trim(), true, true, true);
    }
    if let Some(host_rule) = text.strip_prefix("||") {
        return loose_pattern(host_rule.trim_end_matches('^').trim(), false, false, false);
    }
    if let Some(anchored) = text.strip_prefix('|') {
        // A single leading `|` anchors the address. Subdomains do not match.
        return loose_pattern(anchored.trim_end_matches('^').trim(), false, true, true);
    }
    loose_pattern(text.trim_end_matches('^').trim(), false, false, true)
}

fn looks_like_regular_expression(text: &str) -> bool {
    text.chars().any(|character| {
        matches!(
            character,
            '\\' | '(' | ')' | '[' | ']' | '{' | '}' | '+' | '?' | '|' | '*' | '^' | '$'
        )
    })
}

fn loose_pattern(
    text: &str,
    path_exact: bool,
    host_exact: bool,
    boundary: bool,
) -> Option<Pattern> {
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
    let port = explicit_port(text).or_else(|| parsed.port());
    let explicit_path = parsed.query().is_some() || parsed.path() != "/" || text.ends_with('/');
    if !explicit_path {
        return Some(Pattern::Site {
            host,
            port,
            scheme,
            host_exact,
        });
    }
    let mut path_prefix = parsed.path().to_owned();
    if let Some(query) = parsed.query() {
        path_prefix.push('?');
        path_prefix.push_str(query);
    }
    Some(Pattern::Url {
        host,
        port,
        scheme,
        path_prefix,
        exact: path_exact,
        host_exact,
        boundary,
    })
}

/// The port written in the rule, including defaults such as `:443` that
/// [`Url`] drops while parsing.
fn explicit_port(text: &str) -> Option<u16> {
    let after_scheme = text
        .rsplit_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(text);
    let host = after_scheme.split(['/', '?', '#']).next()?;
    let (_, port) = host.rsplit_once(':')?;
    if port.is_empty() || !port.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    port.parse().ok()
}

fn pattern_matches(pattern: &Pattern, request: &ParsedRequest) -> bool {
    match pattern {
        Pattern::Site {
            host,
            port,
            scheme,
            host_exact,
        } => {
            hosts_agree(&request.host, host, *host_exact) &&
                port_matches(*port, &request.scheme, request.port) &&
                scheme
                    .as_ref()
                    .is_none_or(|scheme| scheme == &request.scheme)
        },
        Pattern::Url {
            host,
            port,
            scheme,
            path_prefix,
            exact,
            host_exact,
            boundary,
        } => {
            if !hosts_agree(&request.host, host, *host_exact) ||
                !port_matches(*port, &request.scheme, request.port)
            {
                return false;
            }
            if let Some(scheme) = scheme &&
                scheme != &request.scheme
            {
                return false;
            }
            path_matches(&request.path_and_query, path_prefix, *exact, *boundary)
        },
        Pattern::Contains(text) => request.path_and_query.contains(text.as_str()),
        Pattern::Glob {
            text,
            case_sensitive,
            domain,
            start,
            end,
        } => {
            let target = glob_target(request, text, *domain);
            glob_matches_site(text, &target, *case_sensitive, *domain, *start, *end)
        },
    }
}

fn hosts_agree(host: &str, rule_host: &str, exact: bool) -> bool {
    if exact {
        host.eq_ignore_ascii_case(rule_host)
    } else {
        host_matches(host, rule_host)
    }
}

fn path_matches(path: &str, prefix: &str, exact: bool, boundary: bool) -> bool {
    if exact || path == prefix {
        return path == prefix;
    }
    if !path.starts_with(prefix) {
        return false;
    }
    if !boundary {
        return true;
    }
    prefix.ends_with('/') || matches!(path.as_bytes().get(prefix.len()), Some(b'/') | Some(b'?'))
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

fn glob_target(request: &ParsedRequest, pattern: &str, domain: bool) -> String {
    let address = if explicit_port(pattern).is_some() &&
        let Some(port) = effective_port(&request.scheme, request.port)
    {
        let mut with_port = request.host.clone();
        with_port.push(':');
        with_port.push_str(&port.to_string());
        with_port.push_str(&request.path_and_query);
        with_port
    } else {
        request.host_and_path.clone()
    };
    if pattern.contains("://") || !domain {
        format!("{}://{address}", request.scheme)
    } else if pattern.contains('/') {
        address
    } else {
        request.host.clone()
    }
}

fn glob_matches_site(
    pattern: &str,
    host_and_path: &str,
    case_sensitive: bool,
    domain: bool,
    start: bool,
    end: bool,
) -> bool {
    let lowered_pattern;
    let lowered_text;
    let (pattern, host_and_path) = if case_sensitive {
        (pattern, host_and_path)
    } else {
        lowered_pattern = pattern.to_ascii_lowercase();
        lowered_text = host_and_path.to_ascii_lowercase();
        (lowered_pattern.as_str(), lowered_text.as_str())
    };
    let mut budget = 20_000u32;
    if domain {
        let mut rest = host_and_path;
        loop {
            if glob_match(pattern, rest, end, &mut budget) {
                return true;
            }
            let Some((before_dot, after_dot)) = rest.split_once('.') else {
                return false;
            };
            // A dot in the path or query is not another hostname label.
            if before_dot.contains('/') || before_dot.contains('?') || after_dot.is_empty() {
                return false;
            }
            rest = after_dot;
        }
    }
    if start {
        return glob_match(pattern, host_and_path, end, &mut budget);
    }
    let mut offset = 0;
    while offset <= host_and_path.len() {
        if glob_match(pattern, &host_and_path[offset..], end, &mut budget) {
            return true;
        }
        offset += host_and_path[offset..]
            .chars()
            .next()
            .map(char::len_utf8)
            .unwrap_or(1);
        if budget == 0 {
            return false;
        }
    }
    false
}

fn is_separator(byte: u8) -> bool {
    !byte.is_ascii_alphanumeric() && !matches!(byte, b'_' | b'-' | b'.' | b'%')
}

/// `*` is a wildcard and `^` is a separator, including the end of the address.
/// `end_anchor` requires the pattern to finish at the end of `text`.
/// `budget` stops a long path from exploring every wildcard split.
fn glob_match(pattern: &str, text: &str, end_anchor: bool, budget: &mut u32) -> bool {
    fn rec(pattern: &[u8], text: &[u8], end_anchor: bool, budget: &mut u32) -> bool {
        if *budget == 0 {
            return false;
        }
        *budget -= 1;
        let mut pattern_index = 0;
        let mut text_index = 0;
        while pattern_index < pattern.len() {
            match pattern[pattern_index] {
                b'*' => {
                    if pattern_index + 1 == pattern.len() {
                        return true;
                    }
                    let mut next = text_index;
                    while next <= text.len() {
                        if rec(
                            &pattern[pattern_index + 1..],
                            &text[next..],
                            end_anchor,
                            budget,
                        ) {
                            return true;
                        }
                        if *budget == 0 || next == text.len() {
                            return false;
                        }
                        next += 1;
                    }
                    return false;
                },
                b'^' => {
                    if text_index == text.len() || is_separator(text[text_index]) {
                        if text_index < text.len() && is_separator(text[text_index]) {
                            text_index += 1;
                        }
                        pattern_index += 1;
                        continue;
                    }
                    return false;
                },
                byte => {
                    if text.get(text_index) != Some(&byte) {
                        return false;
                    }
                    pattern_index += 1;
                    text_index += 1;
                },
            }
        }
        !end_anchor || text_index == text.len()
    }
    rec(pattern.as_bytes(), text.as_bytes(), end_anchor, budget)
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
    if request_host.eq_ignore_ascii_case(source_host) {
        return true;
    }
    let request_site = reg_suffix(request_host);
    let source_site = reg_suffix(source_host);
    !request_site.is_empty() &&
        request_site.eq_ignore_ascii_case(source_site) &&
        !net_traits::pub_domains::is_pub_domain(request_site)
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

fn read_config(path: &Path) -> Result<(bool, Vec<String>, Vec<PathBuf>), ()> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((true, Vec::new(), Vec::new()));
        },
        Err(error) => {
            warn!("Network filter: could not read {}: {error}", path.display());
            return Err(());
        },
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        warn!(
            "Network filter: ignoring unreadable config {}",
            path.display()
        );
        return Err(());
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
    Ok((enabled, whitelist, lists))
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

struct FileStat {
    modified: Option<SystemTime>,
    len: u64,
}

fn watch(path: &Path) -> WatchedFile {
    let stat = file_stat(path);
    WatchedFile {
        path: path.to_path_buf(),
        modified: stat.modified,
        len: stat.len,
    }
}

fn file_stat(path: &Path) -> FileStat {
    match fs::metadata(path) {
        Ok(metadata) => FileStat {
            modified: metadata.modified().ok(),
            len: metadata.len(),
        },
        Err(_) => FileStat {
            modified: None,
            len: 0,
        },
    }
}
