/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! [`DevToolsPanel`] state and event handling.

use std::collections::{HashSet, VecDeque};

use devtools_traits::{
    ComputedNodeLayout, DevtoolScriptControlMsg, EvaluateJSReply, NodeInfo,
};
use servo_base::generic_channel;

use super::types::*;
use super::ipc::{
    with_all_script_channels, with_script_channel, worker_sender,
    WorkerTask,
};

/// All DevTools data, owned exclusively by the UI thread.
pub struct DevToolsState {
    // Console
    pub logs: VecDeque<LogEntry>,
    pub log_filter_level: Option<LogLevel>,
    pub log_search: String,
    pub auto_scroll: bool,
    pub js_input: String,
    pub expanded_logs: HashSet<usize>,

    // Inspector
    pub dom_tree: Vec<DomNode>,
    pub selected_node_path: Vec<usize>,
    pub auto_refresh_dom: bool,
    pub highlighted_node: Option<String>,
    /// Cached CSS style data for the currently selected node.
    pub selected_node_styles: Option<NodeStyleData>,

    // Network
    pub network: VecDeque<NetworkEntry>,
    pub network_search: String,
    pub network_recording: bool,
    pub selected_network: Option<usize>,

    // Application
    pub storage_kind: StorageKind,
    pub local_storage: Vec<StorageItem>,
    pub session_storage: Vec<StorageItem>,
    pub cookies: Vec<CookieEntry>,
    pub selected_storage_key: Option<String>,

    // Performance
    pub frame_times: VecDeque<f64>,
    pub page_timing: Option<PageTiming>,

    // UI
    pub active_tab: DevtoolsTab,
    /// User-controlled height of the bottom dock (px).
    pub bottom_dock_height: f32,
    /// Page title from `TitleChanged` events.
    pub page_title: String,
}

impl Default for DevToolsState {
    fn default() -> Self {
        Self {
            logs: VecDeque::new(),
            log_filter_level: None,
            log_search: String::new(),
            auto_scroll: true,
            js_input: String::new(),
            expanded_logs: HashSet::new(),
            dom_tree: Vec::new(),
            selected_node_path: Vec::new(),
            auto_refresh_dom: false,
            highlighted_node: None,
            selected_node_styles: None,
            network: VecDeque::new(),
            network_search: String::new(),
            network_recording: true,
            selected_network: None,
            storage_kind: StorageKind::default(),
            local_storage: Vec::new(),
            session_storage: Vec::new(),
            cookies: Vec::new(),
            selected_storage_key: None,
            frame_times: VecDeque::new(),
            page_timing: None,
            active_tab: DevtoolsTab::default(),
            bottom_dock_height: 280.0,
            page_title: String::new(),
        }
    }
}

/// DevTools panel controller. Owned by the UI thread (`Gui`).
pub struct DevToolsPanel {
    rx: super::ipc::EventReceiver,
    pub state: DevToolsState,
}

impl DevToolsPanel {
    pub fn new() -> Self {
        Self {
            rx: super::ipc::take_receiver(),
            state: DevToolsState::default(),
        }
    }

    /// Drain all pending events into state. Call once per frame before rendering.
    pub fn drain_events(&mut self) {
        while let Ok(event) = self.rx.try_recv() {
            self.apply_event(event);
        }
    }

    fn apply_event(&mut self, event: DevToolsEvent) {
        match event {
            DevToolsEvent::Console(entry) => {
                self.state.logs.push_back(entry);
                if self.state.logs.len() > MAX_LOG_ENTRIES {
                    self.state.logs.pop_front();
                }
            },
            DevToolsEvent::ClearConsole => {
                self.state.logs.clear();
                self.state.expanded_logs.clear();
            },
            DevToolsEvent::NetworkRequest(entry) => {
                if self.state.network_recording {
                    if let Some(existing) = self
                        .state
                        .network
                        .iter_mut()
                        .find(|e| e.request_id == entry.request_id)
                    {
                        *existing = entry;
                    } else {
                        self.state.network.push_back(entry);
                    }
                    while self.state.network.len() > MAX_NETWORK_ENTRIES {
                        self.state.network.pop_front();
                    }
                }
            },
            DevToolsEvent::NetworkResponse {
                request_id,
                status,
                status_text,
                headers,
                body_size,
                now_ms,
                from_cache,
                mime_type,
                response_body,
                response_is_base64,
                response_cookies,
            } => {
                if let Some(entry) = self
                    .state
                    .network
                    .iter_mut()
                    .find(|e| e.request_id == request_id)
                {
                    entry.status = Some(status);
                    entry.status_text = Some(status_text);
                    entry.response_headers = headers;
                    entry.body_size = body_size;
                    entry.duration_ms = now_ms.saturating_sub(entry.started_ms) as f64;
                    entry.from_cache = from_cache;
                    entry.mime_type = mime_type;
                    entry.response_body = response_body;
                    entry.response_is_base64 = response_is_base64;
                    entry.response_cookies = response_cookies;
                    // Compute receive phase from remaining time.
                    let known = entry.timings.connect_ms + entry.timings.send_ms;
                    entry.timings.receive_ms =
                        (entry.duration_ms - known).max(0.0);
                }
            },
            DevToolsEvent::DomRoot(info) => {
                let root_id = info.unique_id.clone();
                let node = DomNode::from(&info);
                self.state.dom_tree = vec![node];
                self.state.selected_node_path.clear();
                self.state.selected_node_styles = None;
                self.request_dom_children(&root_id);
            },
            DevToolsEvent::DomChildren { parent_id, children } => {
                let new_children: Vec<DomNode> = children.iter().map(DomNode::from).collect();
                let auto_ids: Vec<String> = new_children
                    .iter()
                    .filter(|c| {
                        c.node_type == 1 &&
                            (c.node_name.eq_ignore_ascii_case("html") ||
                                c.node_name.eq_ignore_ascii_case("body"))
                    })
                    .map(|c| c.unique_id.clone())
                    .collect();
                if let Some(parent) = find_node_by_id_mut(&mut self.state.dom_tree, &parent_id) {
                    parent.children = new_children;
                    parent.children_loaded = true;
                }
                for id in auto_ids {
                    self.request_dom_children(&id);
                }
            },
            DevToolsEvent::DomLayout {
                node_id,
                layout,
                auto_margins,
            } => {
                if let Some(node) = find_node_by_id_mut(&mut self.state.dom_tree, &node_id) {
                    node.box_model = Some(*layout);
                    node.auto_margins = auto_margins;
                }
            },
            DevToolsEvent::JsEvalResult(reply) => {
                let rendered = if reply.has_exception {
                    format!(
                        "❌ {}",
                        reply
                            .exception_message
                            .as_deref()
                            .unwrap_or("unknown error")
                    )
                } else {
                    super::ui::utils::format_debugger_value(&reply.value)
                };
                self.state.logs.push_back(LogEntry {
                    timestamp: super::ui::utils::now_timestamp(),
                    level: if reply.has_exception {
                        LogLevel::Error
                    } else {
                        LogLevel::Info
                    },
                    target: "console".into(),
                    message: format!("⟵ {rendered}"),
                    stacktrace: reply.exception_message.clone(),
                });
            },
            DevToolsEvent::Storage {
                kind,
                items,
                cookies,
            } => {
                match kind {
                    StorageKind::LocalStorage => self.state.local_storage = items,
                    StorageKind::SessionStorage => self.state.session_storage = items,
                    StorageKind::Cookies => self.state.cookies = cookies,
                }
            },
            DevToolsEvent::FrameTime(ms) => {
                self.state.frame_times.push_back(ms);
                if self.state.frame_times.len() > MAX_FRAME_SAMPLES {
                    self.state.frame_times.pop_front();
                }
            },
            DevToolsEvent::PageTiming(timing) => {
                self.state.page_timing = Some(*timing);
            },
            DevToolsEvent::NodeStylesLoaded { node_id, data } => {
                // Only accept if this matches the currently selected node.
                if find_node_by_path(&self.state.dom_tree, &self.state.selected_node_path)
                    .is_some_and(|n| n.unique_id == node_id)
                {
                    self.state.selected_node_styles = Some(*data);
                }
            },
            DevToolsEvent::DomAttributeModified {
                node_id,
                attribute_name,
                new_value,
            } => {
                Self::update_dom_attr(
                    &mut self.state.dom_tree,
                    &node_id,
                    &attribute_name,
                    new_value,
                );
            },
            DevToolsEvent::TitleChanged(title) => {
                self.state.page_title = title;
            },
        }
    }

    fn update_dom_attr(
        nodes: &mut [DomNode],
        target_id: &str,
        attr_name: &str,
        new_value: Option<String>,
    ) {
        for node in nodes.iter_mut() {
            if node.unique_id == target_id {
                match (&new_value, node.attrs.iter_mut().find(|a| a.name == attr_name)) {
                    (Some(val), Some(attr)) => attr.value = val.clone(),
                    (Some(val), None) => {
                        node.attrs.push(devtools_traits::AttrInfo {
                            namespace: String::new(),
                            name: attr_name.to_string(),
                            value: val.clone(),
                        });
                    },
                    (None, _) => {
                        node.attrs.retain(|a| a.name != attr_name);
                    },
                }
                return;
            }
            Self::update_dom_attr(&mut node.children, target_id, attr_name, new_value.clone());
        }
    }

    /// Request the root DOM node from the script thread.
    /// Falls back to other registered pipelines if the active one returns `None`.
    pub fn request_dom_tree(&self) {
        let mut sent = false;
        with_script_channel(|script_sender, pipeline_id| {
            let (reply_tx, reply_rx) =
                match generic_channel::channel::<Option<NodeInfo>>() {
                    Some(pair) => pair,
                    None => return,
                };
            let _ = script_sender.send(DevtoolScriptControlMsg::GetRootNode(
                pipeline_id,
                reply_tx,
            ));
            let _ = worker_sender().send(WorkerTask::RootNode {
                pid: pipeline_id,
                rx: reply_rx,
            });
            sent = true;
        });
        if sent {
            return;
        }
        with_all_script_channels(|script_sender, pipeline_id| {
            let (reply_tx, reply_rx) =
                match generic_channel::channel::<Option<NodeInfo>>() {
                    Some(pair) => pair,
                    None => return,
                };
            let _ = script_sender.send(DevtoolScriptControlMsg::GetRootNode(
                pipeline_id,
                reply_tx,
            ));
            let _ = worker_sender().send(WorkerTask::RootNode {
                pid: pipeline_id,
                rx: reply_rx,
            });
        });
    }

    /// Request children for a DOM node (lazy loading).
    pub fn request_dom_children(&self, node_unique_id: &str) {
        let node_id = node_unique_id.to_string();
        with_script_channel(move |script_sender, pipeline_id| {
            let (reply_tx, reply_rx) =
                match generic_channel::channel::<Option<Vec<NodeInfo>>>() {
                    Some(pair) => pair,
                    None => return,
                };
            let _ = script_sender.send(DevtoolScriptControlMsg::GetChildren(
                pipeline_id,
                node_id.clone(),
                reply_tx,
            ));
            let _ = worker_sender().send(WorkerTask::Children {
                parent_id: node_id,
                rx: reply_rx,
            });
        });
    }

    /// Request box-model layout for a DOM node.
    pub fn request_dom_layout(&self, node_unique_id: &str) {
        let node_id = node_unique_id.to_string();
        with_script_channel(move |script_sender, pipeline_id| {
            let (reply_tx, reply_rx) = match generic_channel::channel::<
                Option<(ComputedNodeLayout, devtools_traits::AutoMargins)>,
            >() {
                Some(pair) => pair,
                None => return,
            };
            let _ = script_sender.send(DevtoolScriptControlMsg::GetLayout(
                pipeline_id,
                node_id.clone(),
                reply_tx,
            ));
            let _ = worker_sender().send(WorkerTask::Layout {
                node_id,
                rx: reply_rx,
            });
        });
    }

    /// Evaluate JS via the script thread (`DevtoolScriptControlMsg::Eval`).
    pub fn evaluate_js(&mut self, code: &str) {
        self.state.logs.push_back(LogEntry {
            timestamp: super::ui::utils::now_timestamp(),
            level: LogLevel::Info,
            target: "console".into(),
            message: format!("❯ {code}"),
            stacktrace: None,
        });

        let code_owned = code.to_string();
        with_script_channel(move |script_sender, pipeline_id| {
            let (reply_tx, reply_rx) =
                match generic_channel::channel::<EvaluateJSReply>() {
                    Some(pair) => pair,
                    None => return,
                };
            let _ = script_sender.send(DevtoolScriptControlMsg::Eval(
                code_owned,
                pipeline_id,
                None,
                false,
                reply_tx,
            ));
            let _ = worker_sender().send(WorkerTask::Eval(reply_rx));
        });
    }

    /// Fetch `localStorage`, `sessionStorage`, and `document.cookie`.
    pub fn refresh_storage(&self) {
        with_script_channel(|script_sender, pipeline_id| {
            let (reply_tx, reply_rx) =
                match generic_channel::channel::<EvaluateJSReply>() {
                    Some(pair) => pair,
                    None => return,
                };
            let js = "JSON.stringify({local:Object.entries(localStorage),\
                session:Object.entries(sessionStorage),cookies:document.cookie})";
            let _ = script_sender.send(DevtoolScriptControlMsg::Eval(
                js.to_string(),
                pipeline_id,
                None,
                false,
                reply_tx,
            ));
            let _ = worker_sender().send(WorkerTask::StorageEval(reply_rx));
        });
    }

    /// Query CSS styles, computed styles, and event listeners for a DOM node.
    /// The result arrives asynchronously as [`DevToolsEvent::NodeStylesLoaded`].
    pub fn request_node_styles(&self, node_unique_id: &str) {
        super::ipc::query_node_styles(node_unique_id.to_string());
    }

    /// Modify a CSS rule on a DOM node (live editing).
    pub fn modify_rule(
        &self,
        node_unique_id: &str,
        modifications: Vec<devtools_traits::RuleModification>,
    ) {
        super::ipc::send_modify_rule(node_unique_id, modifications);
    }

    /// Modify a DOM attribute (live editing).
    pub fn modify_attribute(
        &self,
        node_unique_id: &str,
        modifications: Vec<devtools_traits::AttrModification>,
    ) {
        super::ipc::send_modify_attribute(node_unique_id, modifications);
    }

    /// Highlight a DOM node on the rendered web page (deduplicated).
    pub fn highlight_dom_node(&mut self, node_unique_id: &str) {
        if self.state.highlighted_node.as_deref() == Some(node_unique_id) {
            return;
        }
        self.state.highlighted_node = Some(node_unique_id.to_string());
        let node_id = node_unique_id.to_string();
        with_script_channel(move |script_sender, pipeline_id| {
            let _ = script_sender.send(DevtoolScriptControlMsg::HighlightDomNode(
                pipeline_id,
                Some(node_id.clone()),
            ));
        });
    }

    /// Clear any DOM node highlight (only sends IPC if one was active).
    pub fn clear_highlight(&mut self) {
        if self.state.highlighted_node.take().is_none() {
            return;
        }
        with_script_channel(|script_sender, pipeline_id| {
            let _ = script_sender.send(DevtoolScriptControlMsg::HighlightDomNode(
                pipeline_id,
                None,
            ));
        });
    }

    /// Request page load timing from `window.performance.timing`.
    pub fn request_page_timing(&self) {
        with_script_channel(|script_sender, pipeline_id| {
            let (reply_tx, reply_rx) =
                match generic_channel::channel::<EvaluateJSReply>() {
                    Some(pair) => pair,
                    None => return,
                };
            let _ = script_sender.send(DevtoolScriptControlMsg::Eval(
                "JSON.stringify(window.performance.timing)".into(),
                pipeline_id,
                None,
                false,
                reply_tx,
            ));
            let _ = worker_sender().send(WorkerTask::PageTiming(reply_rx));
        });
    }
}

pub(crate) fn find_node_by_id_mut<'a>(
    nodes: &'a mut [DomNode],
    target_id: &str,
) -> Option<&'a mut DomNode> {
    for node in nodes.iter_mut() {
        if node.unique_id == target_id {
            return Some(node);
        }
        if let Some(found) = find_node_by_id_mut(&mut node.children, target_id) {
            return Some(found);
        }
    }
    None
}

/// Find a DOM node by its tree path (list of child indices).
pub(crate) fn find_node_by_path<'a>(nodes: &'a [DomNode], path: &[usize]) -> Option<&'a DomNode> {
    let (&first, rest) = path.split_first()?;
    let node = nodes.get(first)?;
    if rest.is_empty() {
        Some(node)
    } else {
        find_node_by_path(&node.children, rest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devtools::types::{CssPropEntry, CssRuleEntry, NodeStyleData};

    #[test]
    fn node_style_data_default_is_empty() {
        let data = NodeStyleData::default();
        assert!(data.inline_styles.is_empty());
        assert!(data.matched_rules.is_empty());
        assert!(data.computed_styles.is_empty());
        assert!(data.event_listeners.is_empty());
    }

    #[test]
    fn css_prop_entry_from_node_style() {
        let ns = devtools_traits::NodeStyle {
            name: "color".into(),
            value: "red".into(),
            priority: "!important".into(),
        };
        let prop = CssPropEntry::from(ns);
        assert_eq!(prop.name, "color");
        assert_eq!(prop.value, "red");
        assert_eq!(prop.priority, "!important");
        assert!(prop.is_used);
    }

    #[test]
    fn css_rule_entry_holds_declarations() {
        let rule = CssRuleEntry {
            selector: "div.foo".into(),
            href: Some("style.css".into()),
            is_inline: false,
            declarations: vec![CssPropEntry {
                name: "margin".into(),
                value: "0".into(),
                priority: String::new(),
                is_used: true,
            }],
            stylesheet_index: 0,
            block_id: 1,
        };
        assert_eq!(rule.selector, "div.foo");
        assert_eq!(rule.declarations.len(), 1);
        assert!(!rule.is_inline);
    }

    #[test]
    fn dom_node_captures_event_listener_flag() {
        let info = NodeInfo {
            unique_id: "n1".into(),
            host: None,
            base_uri: "http://example.com/".into(),
            parent: String::new(),
            node_type: 1,
            node_name: "button".into(),
            node_value: None,
            num_children: 0,
            attrs: vec![],
            is_top_level_document: false,
            shadow_root_mode: None,
            is_shadow_host: false,
            display: Some("block".into()),
            is_displayed: true,
            doctype_name: None,
            doctype_public_identifier: None,
            doctype_system_identifier: None,
            has_event_listeners: true,
        };
        let node = DomNode::from(&info);
        assert!(node.has_event_listeners);
        assert!(node.doctype.is_none());
    }

    #[test]
    fn update_dom_attr_adds_and_removes() {
        let mut nodes = vec![DomNode {
            unique_id: "n1".into(),
            node_type: 1,
            node_name: "div".into(),
            node_value: None,
            num_children: 0,
            attrs: vec![devtools_traits::AttrInfo {
                namespace: String::new(),
                name: "class".into(),
                value: "old".into(),
            }],
            is_shadow_host: false,
            shadow_root_mode: None,
            display: None,
            is_displayed: true,
            has_event_listeners: false,
            doctype: None,
            children: Vec::new(),
            children_loaded: true,
            box_model: None,
            auto_margins: None,
        }];

        // Update existing attr
        DevToolsPanel::update_dom_attr(
            &mut nodes,
            "n1",
            "class",
            Some("new".into()),
        );
        assert_eq!(nodes[0].attrs[0].value, "new");

        // Add new attr
        DevToolsPanel::update_dom_attr(
            &mut nodes,
            "n1",
            "id",
            Some("foo".into()),
        );
        assert_eq!(nodes[0].attrs.len(), 2);
        assert_eq!(nodes[0].attrs[1].name, "id");

        // Remove attr
        DevToolsPanel::update_dom_attr(&mut nodes, "n1", "class", None);
        assert_eq!(nodes[0].attrs.len(), 1);
        assert_eq!(nodes[0].attrs[0].name, "id");
    }

    #[test]
    fn node_styles_loaded_for_nested_child() {
        use super::super::types::NodeStyleData;
        let mut panel = DevToolsPanel::new();
        // Build a tree: root > html > body > div#target
        let target = DomNode {
            unique_id: "target".into(),
            node_type: 1,
            node_name: "div".into(),
            node_value: None,
            num_children: 0,
            attrs: vec![],
            is_shadow_host: false,
            shadow_root_mode: None,
            display: None,
            is_displayed: true,
            has_event_listeners: false,
            doctype: None,
            children: Vec::new(),
            children_loaded: true,
            box_model: None,
            auto_margins: None,
        };
        let body = DomNode {
            unique_id: "body".into(),
            node_type: 1,
            node_name: "body".into(),
            node_value: None,
            num_children: 1,
            attrs: vec![],
            is_shadow_host: false,
            shadow_root_mode: None,
            display: None,
            is_displayed: true,
            has_event_listeners: false,
            doctype: None,
            children: vec![target],
            children_loaded: true,
            box_model: None,
            auto_margins: None,
        };
        let html = DomNode {
            unique_id: "html".into(),
            node_type: 1,
            node_name: "html".into(),
            node_value: None,
            num_children: 1,
            attrs: vec![],
            is_shadow_host: false,
            shadow_root_mode: None,
            display: None,
            is_displayed: true,
            has_event_listeners: false,
            doctype: None,
            children: vec![body],
            children_loaded: true,
            box_model: None,
            auto_margins: None,
        };
        panel.state.dom_tree = vec![html];
        // Select the nested child node (root > html > body > div).
        panel.state.selected_node_path = vec![0, 0, 0];

        // Simulate styles arriving for the nested child.
        let data = Box::new(NodeStyleData::default());
        panel.apply_event(DevToolsEvent::NodeStylesLoaded {
            node_id: "target".into(),
            data,
        });
        assert!(
            panel.state.selected_node_styles.is_some(),
            "styles for nested child node should be accepted"
        );
    }

    #[test]
    fn multi_property_inline_style_reconstruction() {
        use super::super::types::{CssPropEntry, NodeStyleData};
        let styles = NodeStyleData {
            inline_styles: vec![
                CssPropEntry {
                    name: "color".into(),
                    value: "red".into(),
                    priority: String::new(),
                    is_used: true,
                },
                CssPropEntry {
                    name: "margin".into(),
                    value: "0".into(),
                    priority: "!important".into(),
                    is_used: true,
                },
                CssPropEntry {
                    name: "display".into(),
                    value: "flex".into(),
                    priority: String::new(),
                    is_used: true,
                },
            ],
            ..Default::default()
        };
        let full_style = styles
            .inline_styles
            .iter()
            .map(|p| {
                if p.priority.is_empty() {
                    format!("{}: {}", p.name, p.value)
                } else {
                    format!("{}: {} {}", p.name, p.value, p.priority)
                }
            })
            .collect::<Vec<_>>()
            .join("; ");
        assert_eq!(full_style, "color: red; margin: 0 !important; display: flex");
    }
}
