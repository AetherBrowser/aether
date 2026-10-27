/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Engine-backed DevTools panel.
//!
//! Telemetry is captured by intercepting [`devtools_traits::DevtoolsControlMsg`]
//! in the devtools server thread via [`devtools_traits::set_event_hook`] and
//! forwarded over a lock-free `crossbeam_channel`. DOM / layout / JS queries
//! are issued through the `GenericSender<DevtoolScriptControlMsg>` stored from
//! `ScriptToDevtoolsControlMsg::NewGlobal`. No `evaluate_javascript` is used
//! for telemetry — the only JS evaluation is the Console REPL via
//! [`DevtoolScriptControlMsg::Eval`].

pub(crate) mod types;
pub(crate) mod ipc;
pub(crate) mod panel;
pub(crate) mod ui;

// Public type re-exports.
#[allow(unused_imports)]
pub use types::{
    CookieEntry, CssPropEntry, CssRuleEntry, DevToolsEvent, DevtoolsTab, DomNode, LogEntry,
    LogLevel, NetworkEntry, NodeStyleData, PageTiming, StorageItem, StorageKind,
};

// Public API re-exports.
#[allow(unused_imports)]
pub use ipc::{
    install_engine_hook, register_script_channel, sender, set_active_pipeline,
    set_active_webview, DevToolsSender,
};
#[cfg(feature = "tracing")]
pub use ipc::DevToolsLogLayer;
#[allow(unused_imports)]
pub use panel::{DevToolsPanel, DevToolsState};
#[allow(unused_imports)]
pub use ui::{show_devtools_panel, DevtoolsPanelResult};
