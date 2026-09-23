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
        if let Ok(link) = fs::read_link("/proc/thread-self") &&
            let Some(name) = link.file_name().and_then(|name| name.to_str()) &&
            let Ok(tid) = name.parse()
        {
            return tid;
        }
    }
    #[cfg(target_os = "windows")]
    {
        #[allow(unsafe_code)]
        {
            unsafe extern "system" {
                fn GetCurrentThreadId() -> u32;
            }
            // SAFETY: GetCurrentThreadId is always available on Windows.
            return unsafe { GetCurrentThreadId() };
        }
    }
    #[cfg(target_os = "macos")]
    {
        #[allow(unsafe_code)]
        {
            unsafe extern "C" {
                fn pthread_threadid_np(thread: usize, thread_id: *mut u64) -> i32;
            }
            let mut tid = 0u64;
            // SAFETY: a null pthread_t (0) queries the calling thread.
            if unsafe { pthread_threadid_np(0, &mut tid) } == 0 && tid != 0 {
                return tid as u32;
            }
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        std::process::id()
    }
}

/// Publish (or refresh) stats for the current script thread.
pub fn publish_script_tab_stats(stats: &ScriptTabStats) {
    let dir = stats_dir();
    let _ = fs::create_dir_all(&dir);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
    }
    let Ok(json) = serde_json::to_string(stats) else {
        return;
    };
    let final_path = stats_path(stats.pid, stats.tid);
    if final_path.is_symlink() {
        return;
    }
    let tmp_path = dir.join(format!(
        "{pid}-{tid}.{stamp}.tmp",
        pid = stats.pid,
        tid = stats.tid,
        stamp = now_ms()
    ));
    if fs::write(&tmp_path, json).is_err() {
        return;
    }
    if fs::rename(&tmp_path, &final_path).is_err() {
        let _ = fs::remove_file(&tmp_path);
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
        if path.extension().is_some_and(|extension| extension == "tmp") || path.is_symlink() {
            continue;
        }
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

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    static LOCK: Mutex<()> = Mutex::new(());

    struct Published {
        pid: u32,
        tid: u32,
    }

    impl Drop for Published {
        fn drop(&mut self) {
            unpublish_script_tab_stats(self.pid, self.tid);
        }
    }

    fn unique_ids() -> (u32, u32) {
        // Keep these out of the real PID range so leftover files cannot match a live process.
        static NEXT: Mutex<u32> = Mutex::new(4_000_000);
        let mut next = NEXT.lock().unwrap();
        let pid = *next;
        *next += 1;
        (pid, pid.wrapping_add(1_000_000))
    }

    fn sample(pid: u32, tid: u32, age_ms: u64, heap: u64) -> ScriptTabStats {
        ScriptTabStats {
            pid,
            tid,
            webview_id: format!("wv-{pid}-{tid}"),
            url: "https://example.com/".to_owned(),
            js_heap_bytes: heap,
            updated_ms: unix_time_ms().saturating_sub(age_ms),
        }
    }

    #[test]
    fn publish_then_collect_returns_fresh_stats() {
        let _guard = LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let (pid, tid) = unique_ids();
        let stats = sample(pid, tid, 0, 4096);
        publish_script_tab_stats(&stats);
        let _cleanup = Published { pid, tid };

        let collected = collect_script_tab_stats();
        let found = collected
            .iter()
            .find(|item| item.pid == pid && item.tid == tid)
            .expect("published stats should be collected");
        assert_eq!(found.webview_id, stats.webview_id);
        assert_eq!(found.url, stats.url);
        assert_eq!(found.js_heap_bytes, 4096);
    }

    #[test]
    fn unpublish_removes_stats() {
        let _guard = LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let (pid, tid) = unique_ids();
        publish_script_tab_stats(&sample(pid, tid, 0, 1));
        unpublish_script_tab_stats(pid, tid);
        assert!(
            collect_script_tab_stats()
                .iter()
                .all(|item| item.pid != pid || item.tid != tid)
        );
    }

    #[test]
    fn collect_drops_stale_and_invalid_files() {
        let _guard = LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let (stale_pid, stale_tid) = unique_ids();
        let (bad_pid, bad_tid) = unique_ids();
        publish_script_tab_stats(&sample(stale_pid, stale_tid, STALE_MS + 1_000, 8));
        let _ = fs::create_dir_all(stats_dir());
        fs::write(stats_path(bad_pid, bad_tid), "{not-json").unwrap();

        let collected = collect_script_tab_stats();
        assert!(
            collected
                .iter()
                .all(|item| item.pid != stale_pid && item.pid != bad_pid)
        );
        assert!(!stats_path(stale_pid, stale_tid).exists());
        assert!(!stats_path(bad_pid, bad_tid).exists());
    }

    #[test]
    fn collect_skips_temporary_publish_files() {
        let _guard = LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let (pid, tid) = unique_ids();
        let _ = fs::create_dir_all(stats_dir());
        let tmp_path = stats_dir().join(format!("{pid}-{tid}.123.tmp"));
        fs::write(&tmp_path, "{\"pid\":1}").unwrap();
        let collected = collect_script_tab_stats();
        assert!(
            tmp_path.exists(),
            "temporary files should not be collected or deleted"
        );
        let _ = fs::remove_file(&tmp_path);
        assert!(
            collected
                .iter()
                .all(|item| item.pid != pid || item.tid != tid)
        );
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[test]
    fn current_thread_id_matches_proc() {
        let tid = current_thread_id();
        assert_ne!(tid, 0);
        let link = fs::read_link("/proc/thread-self").unwrap();
        let expected = link
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap()
            .parse::<u32>()
            .unwrap();
        assert_eq!(tid, expected);
    }
}
