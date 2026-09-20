/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Cross-process snapshots of script threads used by `servo:processes`.

use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

const STALE_MS: u64 = 10_000;

/// Live script-thread stats for one tab (or one origin of a tab).
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ScriptTabStats {
    /// Process that owns the script thread.
    pub pid: u32,
    /// OS thread id of the script thread.
    pub tid: u32,
    /// Embedder [`WebViewId`] display string.
    pub webview_id: String,
    /// Top-level document URL on this thread.
    pub url: String,
    /// SpiderMonkey heap size (`JSGC_BYTES`).
    pub js_heap_bytes: u64,
    /// Unix time in milliseconds when this snapshot was written.
    pub updated_ms: u64,
}

fn stats_dir() -> PathBuf {
    std::env::temp_dir().join("aether-tab-stats")
}

fn stats_path(pid: u32, tid: u32) -> PathBuf {
    stats_dir().join(format!("{pid}-{tid}.json"))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

/// Best-effort OS thread id. Falls back to the process id.
pub fn current_thread_id() -> u32 {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        if let Ok(link) = fs::read_link("/proc/thread-self") {
            if let Some(name) = link.file_name().and_then(|name| name.to_str()) {
                if let Ok(tid) = name.parse() {
                    return tid;
                }
            }
        }
    }
    std::process::id()
}

/// Publish (or refresh) stats for the current script thread.
pub fn publish_script_tab_stats(stats: &ScriptTabStats) {
    let _ = fs::create_dir_all(stats_dir());
    if let Ok(json) = serde_json::to_string(stats) {
        let _ = fs::write(stats_path(stats.pid, stats.tid), json);
    }
}

/// Remove stats when a script thread exits.
pub fn unpublish_script_tab_stats(pid: u32, tid: u32) {
    let _ = fs::remove_file(stats_path(pid, tid));
}

/// Collect recent script-thread stats, dropping stale files.
pub fn collect_script_tab_stats() -> Vec<ScriptTabStats> {
    let Ok(entries) = fs::read_dir(stats_dir()) else {
        return Vec::new();
    };
    let now = now_ms();
    let mut stats = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(parsed) = serde_json::from_str::<ScriptTabStats>(&text) else {
            let _ = fs::remove_file(&path);
            continue;
        };
        if now.saturating_sub(parsed.updated_ms) > STALE_MS {
            let _ = fs::remove_file(&path);
            continue;
        }
        stats.push(parsed);
    }
    stats
}

/// Current time in milliseconds since the Unix epoch.
pub fn unix_time_ms() -> u64 {
    now_ms()
}
