/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Core data types for the DevTools panel.

use devtools_traits::{
    AttrInfo, ComputedNodeLayout, EventListenerInfo, NodeInfo, NodeStyle,
};
use egui::Color32;

// Constants

pub(crate) const MAX_LOG_ENTRIES: usize = 2000;
pub(crate) const MAX_NETWORK_ENTRIES: usize = 1000;
pub(crate) const MAX_FRAME_SAMPLES: usize = 300;

// Log / Console

/// Severity of a console / log entry.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub enum LogLevel {
    Error,
    Warn,
    #[default]
    Info,
    Debug,
    Trace,
}

impl LogLevel {
    pub fn color(self) -> Color32 {
        match self {
            LogLevel::Error => Color32::from_rgb(0xF4, 0x43, 0x36),
            LogLevel::Warn => Color32::from_rgb(0xFF, 0x98, 0x00),
            LogLevel::Info => Color32::from_rgb(0x21, 0x96, 0xF3),
            LogLevel::Debug => Color32::from_rgb(0x9E, 0x9E, 0x9E),
            LogLevel::Trace => Color32::from_rgb(0x75, 0x75, 0x75),
        }
    }
}

impl From<devtools_traits::ConsoleLogLevel> for LogLevel {
    fn from(level: devtools_traits::ConsoleLogLevel) -> Self {
        match level {
            devtools_traits::ConsoleLogLevel::Error => LogLevel::Error,
            devtools_traits::ConsoleLogLevel::Warn => LogLevel::Warn,
            devtools_traits::ConsoleLogLevel::Log |
            devtools_traits::ConsoleLogLevel::Info |
            devtools_traits::ConsoleLogLevel::Dir => LogLevel::Info,
            devtools_traits::ConsoleLogLevel::Debug => LogLevel::Debug,
            devtools_traits::ConsoleLogLevel::Trace => LogLevel::Trace,
        }
    }
}

/// One line in the Console tab.
#[derive(Clone)]
pub struct LogEntry {
    pub timestamp: String,
    pub level: LogLevel,
    pub target: String,
    pub message: String,
    pub stacktrace: Option<String>,
}

// Network

/// Network request timing breakdown in milliseconds.
/// Only `connect`, `send`, and `receive` are available from the engine;
/// `receive` is derived as `duration - connect - send` at response time.
#[derive(Clone, Debug, Default)]
pub struct NetworkTimings {
    pub connect_ms: f64,
    pub send_ms: f64,
    pub receive_ms: f64,
}

/// One row in the Network tab, keyed by `request_id: String`.
#[derive(Clone)]
pub struct NetworkEntry {
    pub request_id: String,
    pub url: String,
    pub method: String,
    pub status: Option<u16>,
    pub status_text: Option<String>,
    pub request_headers: Vec<(String, String)>,
    pub response_headers: Vec<(String, String)>,
    pub body_size: u64,
    pub started_ms: u64,
    pub duration_ms: f64,
    pub is_xhr: bool,
    pub from_cache: bool,
    pub failed: bool,
    /// Timing breakdown (connect/send from `HttpRequest`).
    pub timings: NetworkTimings,
    /// Request body (POST data, etc.).
    pub request_body: Option<String>,
    /// Response body as text (UTF-8 lossy) or base64.
    pub response_body: Option<String>,
    /// Whether `response_body` is base64-encoded (binary content).
    pub response_is_base64: bool,
    /// MIME type extracted from response headers.
    pub mime_type: Option<String>,
    /// Resource destination type (e.g. "script", "image", "document").
    pub destination: Option<String>,
    /// Cookies sent with the request (from `Cookie` header).
    pub request_cookies: Vec<CookieEntry>,
    /// Cookies set by the response (from `Set-Cookie` header).
    pub response_cookies: Vec<CookieEntry>,
}

// DOM / Inspector

/// DOM node rendered by the Inspector tab.
/// Maps directly from [`devtools_traits::NodeInfo`].
#[derive(Debug)]
pub struct DomNode {
    pub unique_id: String,
    pub node_type: u16,
    pub node_name: String,
    pub node_value: Option<String>,
    pub num_children: usize,
    pub attrs: Vec<AttrInfo>,
    pub is_shadow_host: bool,
    pub shadow_root_mode: Option<String>,
    pub display: Option<String>,
    pub is_displayed: bool,
    pub has_event_listeners: bool,
    pub doctype: Option<String>,
    pub children: Vec<DomNode>,
    pub children_loaded: bool,
    pub box_model: Option<ComputedNodeLayout>,
    pub auto_margins: Option<(bool, bool, bool, bool)>,
}

impl From<&NodeInfo> for DomNode {
    fn from(info: &NodeInfo) -> Self {
        Self {
            unique_id: info.unique_id.clone(),
            node_type: info.node_type,
            node_name: info.node_name.clone(),
            node_value: info.node_value.clone(),
            num_children: info.num_children,
            attrs: info
                .attrs
                .iter()
                .map(|a| AttrInfo {
                    namespace: a.namespace.clone(),
                    name: a.name.clone(),
                    value: a.value.clone(),
                })
                .collect(),
            is_shadow_host: info.is_shadow_host,
            shadow_root_mode: info.shadow_root_mode.as_ref().map(|m| m.to_string()),
            display: info.display.clone(),
            is_displayed: info.is_displayed,
            has_event_listeners: info.has_event_listeners,
            doctype: info.doctype_name.clone(),
            children: Vec::new(),
            children_loaded: false,
            box_model: None,
            auto_margins: None,
        }
    }
}

// CSS / Styles

/// A single CSS declaration (property: value).
#[derive(Clone, Debug)]
pub struct CssPropEntry {
    pub name: String,
    pub value: String,
    pub priority: String,
    /// Whether this declaration wins the cascade (not overridden by a later rule).
    pub is_used: bool,
}

impl From<NodeStyle> for CssPropEntry {
    fn from(s: NodeStyle) -> Self {
        Self {
            name: s.name,
            value: s.value,
            priority: s.priority,
            is_used: true,
        }
    }
}

/// A matched CSS rule for a DOM node.
#[derive(Clone, Debug)]
pub struct CssRuleEntry {
    /// The selector text (e.g. `"div.foo .bar"`).
    pub selector: String,
    /// Source stylesheet URL (or `None` for inline `style=""`).
    pub href: Option<String>,
    /// Whether this is the inline `style=""` attribute.
    pub is_inline: bool,
    /// Declarations in this rule.
    pub declarations: Vec<CssPropEntry>,
    /// Identity for `ModifyRule` — the `MatchedRule` fields.
    pub stylesheet_index: usize,
    pub block_id: usize,
}

/// Complete style data for a selected DOM node.
#[derive(Clone, Debug, Default)]
pub struct NodeStyleData {
    /// Inline `style=""` attribute declarations.
    pub inline_styles: Vec<CssPropEntry>,
    /// Matched CSS rules (ordered by cascade priority — last wins).
    pub matched_rules: Vec<CssRuleEntry>,
    /// Computed style properties.
    pub computed_styles: Vec<CssPropEntry>,
    /// Event listeners attached to this node.
    pub event_listeners: Vec<EventListenerInfo>,
}

// Application / Storage

/// Which storage backend items came from.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum StorageKind {
    #[default]
    LocalStorage,
    SessionStorage,
    Cookies,
}

/// One key-value pair in the Application tab.
#[derive(Clone)]
pub struct StorageItem {
    pub key: String,
    pub value: String,
}

/// A cookie visible to the Application tab (including `HttpOnly`).
#[derive(Clone)]
pub struct CookieEntry {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub path: String,
    pub http_only: bool,
    pub secure: bool,
    pub same_site: String,
    pub expires: Option<String>,
}

// Performance

/// Page load timing breakdown from `window.performance.timing`.
#[derive(Clone, Debug)]
pub struct PageTiming {
    pub dns_ms: f64,
    pub connect_ms: f64,
    pub ttfb_ms: f64,
    pub dom_interactive_ms: f64,
    pub dom_content_loaded_ms: f64,
    pub load_ms: f64,
}

// Tab selection

/// Which DevTools tab is currently active.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum DevtoolsTab {
    #[default]
    Console,
    Inspector,
    Network,
    Application,
    Performance,
}

// Events

pub enum DevToolsEvent {
    /// A console / log message (from ConsoleAPI or tracing).
    Console(LogEntry),
    /// Clear the console log.
    ClearConsole,
    /// A network request was observed (request phase).
    NetworkRequest(NetworkEntry),
    /// A network response completed for a previously-observed request.
    NetworkResponse {
        request_id: String,
        status: u16,
        status_text: String,
        headers: Vec<(String, String)>,
        body_size: u64,
        now_ms: u64,
        from_cache: bool,
        mime_type: Option<String>,
        response_body: Option<String>,
        response_is_base64: bool,
        response_cookies: Vec<CookieEntry>,
    },
    /// The root DOM node arrived from `GetRootNode`.
    DomRoot(NodeInfo),
    /// Children arrived from `GetChildren` for a parent node.
    DomChildren {
        parent_id: String,
        children: Vec<NodeInfo>,
    },
    /// Box-model layout arrived from `GetLayout`.
    DomLayout {
        node_id: String,
        layout: Box<ComputedNodeLayout>,
        auto_margins: Option<(bool, bool, bool, bool)>,
    },
    /// JS evaluation result from `Eval`.
    JsEvalResult(Box<devtools_traits::EvaluateJSReply>),
    /// Storage / cookie data (from the engine bridge).
    Storage {
        kind: StorageKind,
        items: Vec<StorageItem>,
        cookies: Vec<CookieEntry>,
    },
    /// A single frame-time sample in milliseconds.
    FrameTime(f64),
    /// Page load timing from `window.performance.timing`.
    PageTiming(Box<PageTiming>),
    /// CSS style data loaded for a selected DOM node.
    NodeStylesLoaded {
        node_id: String,
        data: Box<NodeStyleData>,
    },
    /// A DOM attribute was modified on the page.
    DomAttributeModified {
        node_id: String,
        attribute_name: String,
        new_value: Option<String>,
    },
    /// Page title changed.
    TitleChanged(String),
}
