/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Lock-free event channels, async worker, pipeline routing, and engine hooks.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, OnceLock};

use devtools_traits::{
    ChromeToDevtoolsControlMsg, ComputedNodeLayout, DevtoolsControlMsg,
    DevtoolScriptControlMsg, EvaluateJSReply, NavigationState, NetworkEvent, NodeInfo,
    ScriptToDevtoolsControlMsg,
};
use servo::GenericSender;
use servo_base::generic_channel;
use servo_base::generic_channel::GenericReceiver;
use servo_base::id::PipelineId;

use super::types::*;

// Event channel

pub(crate) type EventSender = crossbeam_channel::Sender<DevToolsEvent>;
pub(crate) type EventReceiver = crossbeam_channel::Receiver<DevToolsEvent>;

struct Channel {
    tx: EventSender,
    rx: Mutex<Option<EventReceiver>>,
}

static CHANNEL: OnceLock<Channel> = OnceLock::new();

fn channel() -> &'static Channel {
    CHANNEL.get_or_init(|| {
        let (tx, rx) = crossbeam_channel::unbounded();
        Channel {
            tx,
            rx: Mutex::new(Some(rx)),
        }
    })
}

/// Cloneable, `Send + Sync`, non-blocking event sender.
#[derive(Clone)]
pub struct DevToolsSender {
    tx: EventSender,
}

impl DevToolsSender {
    pub fn send(&self, event: DevToolsEvent) {
        let _ = self.tx.send(event);
    }
}

/// Get (or create) the global [`DevToolsSender`].
pub fn sender() -> DevToolsSender {
    DevToolsSender {
        tx: channel().tx.clone(),
    }
}

/// Take the receiver half of the global channel.
pub(crate) fn take_receiver() -> EventReceiver {
    lock_or_recover(&channel().rx)
        .take()
        .unwrap_or_else(|| {
            let (_tx, rx) = crossbeam_channel::unbounded();
            rx
        })
}

// Mutex helper

pub(crate) fn lock_or_recover<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

// Script pipeline mapping

static SCRIPT_CHANNELS: LazyLock<Mutex<HashMap<PipelineId, GenericSender<DevtoolScriptControlMsg>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static ACTIVE_PIPELINE: Mutex<Option<PipelineId>> = Mutex::new(None);
static WEBVIEW_PIPELINES: LazyLock<Mutex<HashMap<servo::WebViewId, PipelineId>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static ACTIVE_WEBVIEW: Mutex<Option<servo::WebViewId>> = Mutex::new(None);

/// Register the script-thread sender for a [`WebViewId`] (from `NewGlobal`).
pub fn register_script_channel(
    webview_id: servo::WebViewId,
    sender: GenericSender<DevtoolScriptControlMsg>,
    pipeline_id: PipelineId,
    is_about_blank: bool,
) {
    let mut map = lock_or_recover(&SCRIPT_CHANNELS);
    map.insert(pipeline_id, sender);
    let mut webview_map = lock_or_recover(&WEBVIEW_PIPELINES);
    webview_map.insert(webview_id, pipeline_id);
    if !is_about_blank {
        let mut active = lock_or_recover(&ACTIVE_PIPELINE);
        *active = Some(pipeline_id);
    }
}

/// Update the active pipeline (called on `Navigate` → `Stop`).
pub fn set_active_pipeline(pipeline_id: PipelineId) {
    *lock_or_recover(&ACTIVE_PIPELINE) = Some(pipeline_id);
}

/// Set the active [`WebViewId`] (called when the user switches browser tabs).
pub fn set_active_webview(webview_id: Option<servo::WebViewId>) {
    *lock_or_recover(&ACTIVE_WEBVIEW) = webview_id;
    if let Some(wv) = webview_id {
        if let Some(&pid) = lock_or_recover(&WEBVIEW_PIPELINES).get(&wv) {
            *lock_or_recover(&ACTIVE_PIPELINE) = Some(pid);
        }
    }
}

pub(crate) fn active_pipeline_id() -> Option<PipelineId> {
    *lock_or_recover(&ACTIVE_PIPELINE)
}

pub(crate) fn with_script_channel<R>(
    f: impl FnOnce(&GenericSender<DevtoolScriptControlMsg>, PipelineId) -> R,
) -> Option<R> {
    let pid = active_pipeline_id()?;
    let map = lock_or_recover(&SCRIPT_CHANNELS);
    map.get(&pid).map(|sender| f(sender, pid))
}

pub(crate) fn with_all_script_channels<R>(
    mut f: impl FnMut(&GenericSender<DevtoolScriptControlMsg>, PipelineId) -> R,
) -> Vec<R> {
    let map = lock_or_recover(&SCRIPT_CHANNELS);
    map.iter().map(|(&pid, sender)| f(sender, pid)).collect()
}

// Worker thread

pub(crate) enum WorkerTask {
    RootNode {
        pid: PipelineId,
        rx: GenericReceiver<Option<NodeInfo>>,
    },
    Children {
        parent_id: String,
        rx: GenericReceiver<Option<Vec<NodeInfo>>>,
    },
    Layout {
        node_id: String,
        rx: GenericReceiver<Option<(ComputedNodeLayout, devtools_traits::AutoMargins)>>,
    },
    Eval(GenericReceiver<EvaluateJSReply>),
    StorageEval(GenericReceiver<EvaluateJSReply>),
    PageTiming(GenericReceiver<EvaluateJSReply>),
    /// Batched CSS style query — runs on the worker thread.
    NodeStyles {
        node_id: String,
        script_sender: GenericSender<DevtoolScriptControlMsg>,
        pipeline_id: PipelineId,
    },
}

static WORKER: OnceLock<crossbeam_channel::Sender<WorkerTask>> = OnceLock::new();

pub(crate) fn worker_sender() -> &'static crossbeam_channel::Sender<WorkerTask> {
    WORKER.get_or_init(|| {
        let (tx, rx) = crossbeam_channel::unbounded();
        let _ = std::thread::Builder::new()
            .name("DevToolsWorker".into())
            .spawn(move || {
                let s = sender();
                while let Ok(task) = rx.recv() {
                    match task {
                        WorkerTask::RootNode { pid, rx } => {
                            if let Ok(Some(node)) = rx.recv() {
                                *lock_or_recover(&ACTIVE_PIPELINE) = Some(pid);
                                s.send(DevToolsEvent::DomRoot(node));
                            } else {
                                // Fallback: try all other registered pipelines.
                                let map = lock_or_recover(&SCRIPT_CHANNELS);
                                let candidates: Vec<_> = map
                                    .iter()
                                    .filter(|entry| entry.0 != &pid)
                                    .map(|(&p, sender)| (p, sender.clone()))
                                    .collect();
                                drop(map);
                                for (other_pid, other_sender) in candidates {
                                    let (reply_tx, reply_rx) =
                                        match generic_channel::channel::<Option<NodeInfo>>() {
                                            Some(pair) => pair,
                                            None => continue,
                                        };
                                    let _ = other_sender.send(
                                        DevtoolScriptControlMsg::GetRootNode(other_pid, reply_tx),
                                    );
                                    if let Ok(Some(node)) = reply_rx.recv() {
                                        *lock_or_recover(&ACTIVE_PIPELINE) = Some(other_pid);
                                        s.send(DevToolsEvent::DomRoot(node));
                                        break;
                                    }
                                }
                            }
                        },
                        WorkerTask::Children { parent_id, rx } => {
                            if let Ok(Some(children)) = rx.recv() {
                                s.send(DevToolsEvent::DomChildren { parent_id, children });
                            }
                        },
                        WorkerTask::Layout { node_id, rx } => {
                            if let Ok(Some((layout, auto_margins))) = rx.recv() {
                                let auto = (
                                    auto_margins.top,
                                    auto_margins.right,
                                    auto_margins.bottom,
                                    auto_margins.left,
                                );
                                s.send(DevToolsEvent::DomLayout {
                                    node_id,
                                    layout: Box::new(layout),
                                    auto_margins: Some(auto),
                                });
                            }
                        },
                        WorkerTask::Eval(reply_rx) => {
                            if let Ok(reply) = reply_rx.recv() {
                                s.send(DevToolsEvent::JsEvalResult(Box::new(reply)));
                            }
                        },
                        WorkerTask::StorageEval(reply_rx) => {
                            if let Ok(reply) = reply_rx.recv() {
                                handle_storage_eval_reply(&s, &reply);
                            }
                        },
                        WorkerTask::PageTiming(reply_rx) => {
                            if let Ok(reply) = reply_rx.recv() {
                                handle_page_timing_reply(&s, &reply);
                            }
                        },
                        WorkerTask::NodeStyles { node_id, script_sender, pipeline_id } => {
                            let data = fetch_node_styles(&script_sender, pipeline_id, &node_id);
                            s.send(DevToolsEvent::NodeStylesLoaded {
                                node_id,
                                data: Box::new(data),
                            });
                        },
                    }
                }
            });
        tx
    })
}

// Reply handlers

fn handle_storage_eval_reply(s: &DevToolsSender, reply: &EvaluateJSReply) {
    let devtools_traits::DebuggerValue::StringValue(json) = &reply.value else {
        return;
    };
    let Ok(val) = serde_json::from_str::<serde_json::Value>(json) else {
        return;
    };
    let parse_items = |key: &str| -> Vec<StorageItem> {
        val.get(key)
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|p| {
                        let p = p.as_array()?;
                        Some(StorageItem {
                            key: p.first()?.as_str()?.to_string(),
                            value: p
                                .get(1)
                                .map(|v| match v {
                                    serde_json::Value::String(s) => s.clone(),
                                    other => other.to_string(),
                                })
                                .unwrap_or_default(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    let cookies_str = val.get("cookies").and_then(|v| v.as_str()).unwrap_or("");
    let cookies: Vec<CookieEntry> = cookies_str
        .split(';')
        .filter_map(|s| {
            let s = s.trim();
            if s.is_empty() {
                return None;
            }
            let (name, value) = s.split_once('=')?;
            Some(CookieEntry {
                name: name.trim().to_string(),
                value: value.trim().to_string(),
                domain: String::new(),
                path: "/".into(),
                http_only: false,
                secure: false,
                same_site: String::new(),
                expires: None,
            })
        })
        .collect();
    s.send(DevToolsEvent::Storage {
        kind: StorageKind::LocalStorage,
        items: parse_items("local"),
        cookies: Vec::new(),
    });
    s.send(DevToolsEvent::Storage {
        kind: StorageKind::SessionStorage,
        items: parse_items("session"),
        cookies: Vec::new(),
    });
    s.send(DevToolsEvent::Storage {
        kind: StorageKind::Cookies,
        items: Vec::new(),
        cookies,
    });
}

fn handle_page_timing_reply(s: &DevToolsSender, reply: &EvaluateJSReply) {
    let devtools_traits::DebuggerValue::StringValue(json) = &reply.value else {
        return;
    };
    let Ok(val) = serde_json::from_str::<serde_json::Value>(json) else {
        return;
    };
    let get_ms = |key: &str| -> f64 {
        val.get(key).and_then(|v| v.as_f64()).unwrap_or(0.0)
    };
    let navigation_start = get_ms("navigationStart");
    let dns = get_ms("domainLookupEnd") - get_ms("domainLookupStart");
    let connect = get_ms("connectEnd") - get_ms("connectStart");
    let ttfb = get_ms("responseStart") - navigation_start;
    let dom_interactive = get_ms("domInteractive") - navigation_start;
    let dom_content_loaded = get_ms("domContentLoadedEventEnd") - navigation_start;
    let load = get_ms("loadEventEnd") - navigation_start;
    s.send(DevToolsEvent::PageTiming(Box::new(PageTiming {
        dns_ms: dns.max(0.0),
        connect_ms: connect.max(0.0),
        ttfb_ms: ttfb.max(0.0),
        dom_interactive_ms: dom_interactive.max(0.0),
        dom_content_loaded_ms: dom_content_loaded.max(0.0),
        load_ms: load.max(0.0),
    })));
}

// CSS style queries

/// Convert epoch milliseconds to `HH:MM:SS.mmm` string.
fn epoch_ms_to_hms(epoch_ms: u64) -> String {
    let secs = (epoch_ms / 1000) % 86400;
    let ms = epoch_ms % 1000;
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    format!("{h:02}:{m:02}:{s:02}.{ms:03}")
}

/// Batch-query all CSS style data for a DOM node via the existing DevTools worker thread.
pub(crate) fn query_node_styles(node_id: String) {
    let Some((script_sender, pipeline_id)) = with_script_channel(|s, pid| (s.clone(), pid)) else {
        return;
    };
    let _ = worker_sender().send(WorkerTask::NodeStyles {
        node_id,
        script_sender,
        pipeline_id,
    });
}

fn fetch_node_styles(
    script_sender: &GenericSender<DevtoolScriptControlMsg>,
    pipeline_id: PipelineId,
    node_id: &str,
) -> NodeStyleData {
    let mut data = NodeStyleData::default();

    // 1. Inline `style=""` attribute declarations.
    if let Some(styles) = query_generic(script_sender, |tx| {
        DevtoolScriptControlMsg::GetAttributeStyle(pipeline_id, node_id.to_string(), tx)
    }) {
        data.inline_styles = styles.into_iter().map(CssPropEntry::from).collect();
    }

    // 2. Matched CSS rules → fetch declarations per rule.
    if let Some(matched_rules) = query_generic(script_sender, |tx| {
        DevtoolScriptControlMsg::GetSelectors(pipeline_id, node_id.to_string(), tx)
    }) {
        for mr in matched_rules {
            let declarations: Vec<CssPropEntry> = query_generic(script_sender, |tx| {
                DevtoolScriptControlMsg::GetStylesheetStyle(
                    pipeline_id,
                    node_id.to_string(),
                    mr.clone(),
                    tx,
                )
            })
            .unwrap_or_default()
            .into_iter()
            .map(CssPropEntry::from)
            .collect();
            data.matched_rules.push(CssRuleEntry {
                selector: mr.selector.clone(),
                href: None,
                is_inline: false,
                declarations,
                stylesheet_index: mr.stylesheet_index,
                block_id: mr.block_id,
            });
        }
    }

    // 3. Computed styles.
    if let Some(computed) = query_generic(script_sender, |tx| {
        DevtoolScriptControlMsg::GetComputedStyle(pipeline_id, node_id.to_string(), tx)
    }) {
        data.computed_styles = computed.into_iter().map(CssPropEntry::from).collect();
    }

    // 4. Event listeners (returns Vec, not Option).
    if let Some(listeners) = query_generic_vec(script_sender, |tx| {
        DevtoolScriptControlMsg::GetEventListenerInfo(pipeline_id, node_id.to_string(), tx)
    }) {
        data.event_listeners = listeners;
    }

    // Compute cascade: later rules override earlier ones for the same property.
    compute_cascade_override(&mut data);

    data
}

/// Mark `is_used = false` on declarations overridden by a later rule or inline style.
fn compute_cascade_override(data: &mut NodeStyleData) {
    use std::collections::HashSet;
    let mut winners: HashSet<String> = HashSet::new();

    // Inline styles always win.
    for prop in &mut data.inline_styles {
        prop.is_used = true;
        winners.insert(prop.name.clone());
    }

    // Process rules in reverse — later rules override earlier ones.
    for rule in data.matched_rules.iter_mut().rev() {
        for prop in &mut rule.declarations {
            if winners.contains(&prop.name) {
                prop.is_used = false;
            } else {
                prop.is_used = true;
                winners.insert(prop.name.clone());
            }
        }
    }
}

/// Send a `DevtoolScriptControlMsg` that expects `Option<T>` back and wait for the reply.
fn query_generic<T>(
    script_sender: &GenericSender<DevtoolScriptControlMsg>,
    make_msg: impl FnOnce(GenericSender<Option<T>>) -> DevtoolScriptControlMsg,
) -> Option<T>
where
    T: serde::Serialize + serde::de::DeserializeOwned + Send + 'static,
{
    let (tx, rx) = generic_channel::channel::<Option<T>>()?;
    let _ = script_sender.send(make_msg(tx));
    rx.recv().ok().flatten()
}

/// Send a `DevtoolScriptControlMsg` that expects `Vec<T>` back and wait for the reply.
fn query_generic_vec<T>(
    script_sender: &GenericSender<DevtoolScriptControlMsg>,
    make_msg: impl FnOnce(GenericSender<Vec<T>>) -> DevtoolScriptControlMsg,
) -> Option<Vec<T>>
where
    T: serde::Serialize + serde::de::DeserializeOwned + Send + 'static,
{
    let (tx, rx) = generic_channel::channel::<Vec<T>>()?;
    let _ = script_sender.send(make_msg(tx));
    rx.recv().ok()
}

/// Send a `ModifyRule` write-back to the script thread (fire-and-forget).
pub(crate) fn send_modify_rule(
    node_id: &str,
    modifications: Vec<devtools_traits::RuleModification>,
) {
    let node_id = node_id.to_string();
    with_script_channel(move |script_sender, pipeline_id| {
        let _ = script_sender.send(DevtoolScriptControlMsg::ModifyRule(
            pipeline_id,
            node_id.clone(),
            modifications,
        ));
    });
}

/// Send a `ModifyAttribute` write-back to the script thread (fire-and-forget).
pub(crate) fn send_modify_attribute(
    node_id: &str,
    modifications: Vec<devtools_traits::AttrModification>,
) {
    let node_id = node_id.to_string();
    with_script_channel(move |script_sender, pipeline_id| {
        let _ = script_sender.send(DevtoolScriptControlMsg::ModifyAttribute(
            pipeline_id,
            node_id.clone(),
            modifications,
        ));
    });
}

// Engine hook

/// Install the global hook that intercepts `DevtoolsControlMsg` from the
/// devtools server thread and forwards telemetry to [`sender()`].
///
/// Call this once during embedder startup (before `Servo::new`).
pub fn install_engine_hook() {
    devtools_traits::set_event_hook(|msg: &DevtoolsControlMsg| {
        let s = sender();
        match msg {
            DevtoolsControlMsg::FromChrome(ChromeToDevtoolsControlMsg::NetworkEvent(
                request_id,
                event,
            )) => {
                handle_network_event(&s, request_id, event);
            },
            DevtoolsControlMsg::FromScript(ScriptToDevtoolsControlMsg::ConsoleAPI(
                _pipeline_id,
                console_msg,
                _worker_id,
            )) => {
                let fields = &console_msg.fields;
                let args: Vec<String> = console_msg
                    .arguments
                    .iter()
                    .map(super::ui::utils::format_debugger_value)
                    .collect();
                // Use engine timestamp (ms since epoch) when available, fall back to wall clock.
                let timestamp = if fields.time_stamp > 0 {
                    epoch_ms_to_hms(fields.time_stamp)
                } else {
                    super::ui::utils::now_timestamp()
                };
                s.send(DevToolsEvent::Console(LogEntry {
                    timestamp,
                    level: fields.level.clone().into(),
                    target: format!(
                        "{}:{}:{}",
                        fields.filename, fields.line_number, fields.column_number
                    ),
                    message: args.join(" "),
                    stacktrace: console_msg.stacktrace.as_ref().map(|st| {
                        st.iter()
                            .map(|f| {
                                format!(
                                    "{} ({}:{}:{})",
                                    f.function_name, f.filename, f.line_number, f.column_number
                                )
                            })
                            .collect::<Vec<_>>()
                            .join("\n")
                    }),
                }));
            },
            DevtoolsControlMsg::FromScript(ScriptToDevtoolsControlMsg::NewGlobal(
                ids,
                script_sender,
                page_info,
            )) => {
                let (_browsing_context_id, pipeline_id, worker_id, webview_id) = ids;
                if worker_id.is_none() && page_info.is_top_level_global {
                    let is_about_blank = page_info.url.as_str().starts_with("about:blank");
                    register_script_channel(
                        *webview_id,
                        script_sender.clone(),
                        *pipeline_id,
                        is_about_blank,
                    );
                    let _ = script_sender.send(DevtoolScriptControlMsg::WantsLiveNotifications(
                        *pipeline_id,
                        true,
                    ));
                }
            },
            DevtoolsControlMsg::FromScript(ScriptToDevtoolsControlMsg::Navigate(
                _bc_id,
                NavigationState::Stop(pipeline_id, _page_info),
            )) => {
                set_active_pipeline(*pipeline_id);
            },
            DevtoolsControlMsg::FromScript(ScriptToDevtoolsControlMsg::Navigate(
                _bc_id,
                NavigationState::Start(_url),
            )) => {
                // Navigation started — no panel state change needed.
            },
            DevtoolsControlMsg::FromScript(ScriptToDevtoolsControlMsg::ClearConsole(
                _pipeline_id,
                _worker_id,
            )) => {
                s.send(DevToolsEvent::ClearConsole);
            },
            DevtoolsControlMsg::FromScript(ScriptToDevtoolsControlMsg::FramerateTick(
                _actor_name,
                tick,
            )) => {
                s.send(DevToolsEvent::FrameTime(*tick));
            },
            DevtoolsControlMsg::FromScript(ScriptToDevtoolsControlMsg::DomMutation(
                _pipeline_id,
                mutation,
            )) => {
                let devtools_traits::DomMutation::AttributeModified {
                    node,
                    attribute_name,
                    new_value,
                } = mutation;
                s.send(DevToolsEvent::DomAttributeModified {
                    node_id: node.clone(),
                    attribute_name: attribute_name.clone(),
                    new_value: new_value.clone(),
                });
            },
            DevtoolsControlMsg::FromScript(ScriptToDevtoolsControlMsg::TitleChanged(
                _pipeline_id,
                title,
            )) => {
                s.send(DevToolsEvent::TitleChanged(title.clone()));
            },
            DevtoolsControlMsg::FromScript(ScriptToDevtoolsControlMsg::ReportPageError(
                _pipeline_id,
                page_error,
            )) => {
                s.send(DevToolsEvent::Console(LogEntry {
                    timestamp: super::ui::utils::now_timestamp(),
                    level: LogLevel::Error,
                    target: format!(
                        "{}:{}:{}",
                        page_error.source_name, page_error.line_number, page_error.column_number
                    ),
                    message: page_error.error_message.clone(),
                    stacktrace: None,
                }));
            },
            DevtoolsControlMsg::FromScript(ScriptToDevtoolsControlMsg::ReportCSSError(
                _pipeline_id,
                css_error,
            )) => {
                s.send(DevToolsEvent::Console(LogEntry {
                    timestamp: super::ui::utils::now_timestamp(),
                    level: LogLevel::Warn,
                    target: format!(
                        "{}:{}:{}",
                        css_error.filename, css_error.line, css_error.column
                    ),
                    message: css_error.msg.clone(),
                    stacktrace: None,
                }));
            },
            _ => {},
        }
    });
}

fn handle_network_event(s: &DevToolsSender, request_id: &str, event: &NetworkEvent) {
    match event {
        NetworkEvent::HttpRequest(req) | NetworkEvent::HttpRequestUpdate(req) => {
            let headers: Vec<(String, String)> = req
                .headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
                .collect();
            let started_ms = req
                .started_date_time
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            let request_cookies = parse_cookie_header(&req.headers);
            let request_body = req.body.as_ref().map(|b| {
                String::from_utf8_lossy(b).to_string()
            });
            let destination = format!("{:?}", req.destination);
            s.send(DevToolsEvent::NetworkRequest(NetworkEntry {
                request_id: request_id.to_string(),
                url: req.url.as_str().to_string(),
                method: req.method.as_str().to_string(),
                status: None,
                status_text: None,
                request_headers: headers,
                response_headers: Vec::new(),
                body_size: 0,
                started_ms,
                duration_ms: 0.0,
                is_xhr: req.is_xhr,
                from_cache: false,
                failed: false,
                timings: NetworkTimings {
                    connect_ms: req.connect_time.as_millis() as f64,
                    send_ms: req.send_time.as_millis() as f64,
                    receive_ms: 0.0,
                },
                request_body,
                response_body: None,
                response_is_base64: false,
                mime_type: None,
                destination: Some(destination),
                request_cookies,
                response_cookies: Vec::new(),
            }));
        },
        NetworkEvent::HttpResponse(resp) => {
            let headers: Vec<(String, String)> = resp
                .headers
                .as_ref()
                .map(|h| {
                    h.iter()
                        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
                        .collect()
                })
                .unwrap_or_default();
            let body_size = resp.body.as_ref().map(|b| b.len() as u64).unwrap_or(0);
            let status_code = resp.status.code().as_u16();

            // Extract MIME type from response headers.
            let mime_type = resp.headers.as_ref().and_then(|h| {
                h.get(http::header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .map(|s| s.to_string())
            });

            // Decode response body: text vs base64.
            let (response_body, response_is_base64) = match &resp.body {
                Some(body) if body.len() > 0 => {
                    let is_text = mime_type.as_deref().map(|m| {
                        m.starts_with("text/") ||
                            m.contains("json") ||
                            m.contains("javascript") ||
                            m.contains("xml") ||
                            m.contains("html") ||
                            m.contains("css")
                    }).unwrap_or(true);
                    if is_text {
                        (Some(String::from_utf8_lossy(body).to_string()), false)
                    } else {
                        use base64::Engine;
                        (
                            Some(base64::engine::general_purpose::STANDARD.encode(body)),
                            true,
                        )
                    }
                },
                _ => (None, false),
            };

            let response_cookies = parse_set_cookie_headers(resp.headers.as_ref());

            s.send(DevToolsEvent::NetworkResponse {
                request_id: request_id.to_string(),
                status: status_code,
                status_text: resp
                    .status
                    .code()
                    .canonical_reason()
                    .unwrap_or("")
                    .to_string(),
                headers,
                body_size,
                now_ms: devtools_traits::get_time_stamp(),
                from_cache: resp.from_cache,
                mime_type,
                response_body,
                response_is_base64,
                response_cookies,
            });
        },
        NetworkEvent::SecurityInfo(_) => {},
    }
}

/// Parse `Cookie` request header into structured entries.
fn parse_cookie_header(headers: &http::HeaderMap) -> Vec<CookieEntry> {
    headers
        .get(http::header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .map(|s| {
            s.split(';')
                .filter_map(|pair| {
                    let (name, value) = pair.trim().split_once('=')?;
                    Some(CookieEntry {
                        name: name.trim().to_string(),
                        value: value.trim().to_string(),
                        domain: String::new(),
                        path: "/".into(),
                        http_only: false,
                        secure: false,
                        same_site: String::new(),
                        expires: None,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Parse `Set-Cookie` response headers into structured entries.
fn parse_set_cookie_headers(headers: Option<&http::HeaderMap>) -> Vec<CookieEntry> {
    let Some(headers) = headers else {
        return Vec::new();
    };
    headers
        .get_all(http::header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .filter_map(|s| {
            let mut parts = s.split(';');
            let (name, value) = parts.next()?.trim().split_once('=')?;
            let mut entry = CookieEntry {
                name: name.trim().to_string(),
                value: value.trim().to_string(),
                domain: String::new(),
                path: "/".into(),
                http_only: false,
                secure: false,
                same_site: String::new(),
                expires: None,
            };
            for attr in parts {
                let attr = attr.trim();
                let lower = attr.to_ascii_lowercase();
                if lower.starts_with("domain=") {
                    entry.domain = attr[7..].to_string();
                } else if lower.starts_with("path=") {
                    entry.path = attr[5..].to_string();
                } else if lower.starts_with("expires=") {
                    entry.expires = Some(attr[8..].to_string());
                } else if lower == "httponly" {
                    entry.http_only = true;
                } else if lower == "secure" {
                    entry.secure = true;
                } else if lower.starts_with("samesite=") {
                    entry.same_site = attr[9..].to_string();
                }
            }
            Some(entry)
        })
        .collect()
}

// Tracing layer

/// A [`tracing_subscriber::Layer`] that forwards engine tracing events.
#[cfg(feature = "tracing")]
pub struct DevToolsLogLayer {
    sender: DevToolsSender,
}

#[cfg(feature = "tracing")]
impl DevToolsLogLayer {
    pub fn new(sender: DevToolsSender) -> Self {
        Self { sender }
    }
}

#[cfg(feature = "tracing")]
impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for DevToolsLogLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: tracing_subscriber::layer::Context<'_, S>) {
        use tracing::field::Visit;
        use tracing::Level;

        struct MessageVisitor {
            message: String,
        }

        impl Visit for MessageVisitor {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "message" {
                    self.message = format!("{value:?}");
                }
            }

            fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
                if field.name() == "message" {
                    self.message = value.to_string();
                }
            }
        }

        let meta = event.metadata();
        let mut visitor = MessageVisitor {
            message: String::new(),
        };
        event.record(&mut visitor);

        if visitor.message.is_empty() || meta.target().starts_with("devtools") {
            return;
        }

        let level = match *meta.level() {
            Level::ERROR => LogLevel::Error,
            Level::WARN => LogLevel::Warn,
            Level::INFO => LogLevel::Info,
            Level::DEBUG => LogLevel::Debug,
            Level::TRACE => LogLevel::Trace,
        };

        self.sender.send(DevToolsEvent::Console(LogEntry {
            timestamp: super::ui::utils::now_timestamp(),
            level,
            target: meta.target().to_string(),
            message: visitor.message,
            stacktrace: None,
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_cookie_header_basic() {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::COOKIE,
            http::HeaderValue::from_static("session=abc123; theme=dark"),
        );
        let cookies = parse_cookie_header(&headers);
        assert_eq!(cookies.len(), 2);
        assert_eq!(cookies[0].name, "session");
        assert_eq!(cookies[0].value, "abc123");
        assert_eq!(cookies[1].name, "theme");
        assert_eq!(cookies[1].value, "dark");
    }

    #[test]
    fn parse_set_cookie_headers_with_attrs() {
        let mut headers = http::HeaderMap::new();
        headers.append(
            http::header::SET_COOKIE,
            http::HeaderValue::from_static(
                "token=xyz; Domain=example.com; Path=/; HttpOnly; Secure; SameSite=Strict",
            ),
        );
        let cookies = parse_set_cookie_headers(Some(&headers));
        assert_eq!(cookies.len(), 1);
        let c = &cookies[0];
        assert_eq!(c.name, "token");
        assert_eq!(c.value, "xyz");
        assert_eq!(c.domain, "example.com");
        assert_eq!(c.path, "/");
        assert!(c.http_only);
        assert!(c.secure);
        assert_eq!(c.same_site, "Strict");
    }

    #[test]
    fn parse_set_cookie_headers_multiple() {
        let mut headers = http::HeaderMap::new();
        headers.append(
            http::header::SET_COOKIE,
            http::HeaderValue::from_static("a=1"),
        );
        headers.append(
            http::header::SET_COOKIE,
            http::HeaderValue::from_static("b=2; HttpOnly"),
        );
        let cookies = parse_set_cookie_headers(Some(&headers));
        assert_eq!(cookies.len(), 2);
        assert_eq!(cookies[0].name, "a");
        assert_eq!(cookies[1].name, "b");
        assert!(cookies[1].http_only);
    }

    #[test]
    fn network_timings_fields() {
        let t = NetworkTimings {
            connect_ms: 30.0,
            send_ms: 5.0,
            receive_ms: 100.0,
        };
        assert_eq!(t.connect_ms, 30.0);
        assert_eq!(t.send_ms, 5.0);
        assert_eq!(t.receive_ms, 100.0);
    }

    #[test]
    fn network_entry_default_timings() {
        let entry = NetworkEntry {
            request_id: "r1".into(),
            url: "http://example.com/".into(),
            method: "GET".into(),
            status: None,
            status_text: None,
            request_headers: Vec::new(),
            response_headers: Vec::new(),
            body_size: 0,
            started_ms: 0,
            duration_ms: 0.0,
            is_xhr: false,
            from_cache: false,
            failed: false,
            timings: NetworkTimings::default(),
            request_body: None,
            response_body: None,
            response_is_base64: false,
            mime_type: None,
            destination: None,
            request_cookies: Vec::new(),
            response_cookies: Vec::new(),
        };
        assert_eq!(entry.timings.connect_ms, 0.0);
        assert!(entry.request_body.is_none());
        assert!(entry.response_body.is_none());
    }
}
