/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Snapshot of the browser process tree for `servo:processes`.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Mutex;
use std::time::Instant;

use serde_json::{Value, json};
use servo::profile_traits::tab_stats::collect_script_tab_stats;

static EMBEDDER_TABS: Mutex<Vec<EmbedderTab>> = Mutex::new(Vec::new());

/// Chrome-side tab metadata published from the embedder event loop.
#[derive(Clone)]
pub(crate) struct EmbedderTab {
    pub webview_id: String,
    pub title: String,
    pub url: String,
}

/// Replace the list of open tabs shown on `servo:processes`.
pub(crate) fn publish_embedder_tabs(tabs: Vec<EmbedderTab>) {
    match EMBEDDER_TABS.lock() {
        Ok(mut current) => *current = tabs,
        Err(poisoned) => *poisoned.into_inner() = tabs,
    }
}

fn embedder_tabs() -> Vec<EmbedderTab> {
    match EMBEDDER_TABS.lock() {
        Ok(current) => current.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    }
}

#[derive(Default)]
pub(crate) struct ProcessSampler {
    last: Option<(Instant, HashMap<SampleKey, u64>)>,
}

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
enum SampleKey {
    Process(u32),
    Thread(u32, u32),
}

struct RawThread {
    tid: u32,
    name: String,
    cpu_ticks: u64,
}

struct RawProcess {
    pid: u32,
    ppid: u32,
    name: String,
    memory: Option<u64>,
    cpu_ticks: u64,
    threads: Vec<RawThread>,
}

impl ProcessSampler {
    pub(crate) fn snapshot_json(&mut self) -> String {
        let mut processes = collect_processes();
        let now = Instant::now();
        let previous = self.last.take();
        let elapsed = previous
            .as_ref()
            .map(|(time, _)| now.saturating_duration_since(*time).as_secs_f64())
            .unwrap_or(0.0);
        let clk_tck = clock_ticks_per_second();
        let cores = cpu_core_count();

        let mut current_samples = HashMap::new();
        let mut json_processes = Vec::new();
        for process in &mut processes {
            process
                .threads
                .sort_by(|a, b| b.cpu_ticks.cmp(&a.cpu_ticks).then(a.name.cmp(&b.name)));
            current_samples.insert(SampleKey::Process(process.pid), process.cpu_ticks);
            let cpu = cpu_percent(
                previous.as_ref().map(|(_, samples)| samples),
                SampleKey::Process(process.pid),
                process.cpu_ticks,
                elapsed,
                clk_tck,
                cores,
            );
            let threads: Vec<Value> = process
                .threads
                .iter()
                .map(|thread| {
                    current_samples
                        .insert(SampleKey::Thread(process.pid, thread.tid), thread.cpu_ticks);
                    json!({
                        "tid": thread.tid,
                        "name": thread.name,
                        "cpu": cpu_percent(
                            previous.as_ref().map(|(_, samples)| samples),
                            SampleKey::Thread(process.pid, thread.tid),
                            thread.cpu_ticks,
                            elapsed,
                            clk_tck,
                            cores,
                        ),
                    })
                })
                .collect();
            json_processes.push(json!({
                "pid": process.pid,
                "name": process.name,
                "memory": process.memory,
                "cpu": cpu,
                "threads": threads,
                "tabs": [],
            }));
        }

        attach_tabs(&mut json_processes);

        json_processes.sort_by(|a, b| {
            let memory_a = a["memory"].as_u64().unwrap_or(0);
            let memory_b = b["memory"].as_u64().unwrap_or(0);
            memory_b.cmp(&memory_a)
        });

        self.last = Some((now, current_samples));
        json!({
            "cores": cores,
            "processes": json_processes,
        })
        .to_string()
    }
}

fn cpu_percent(
    previous: Option<&HashMap<SampleKey, u64>>,
    key: SampleKey,
    cpu_ticks: u64,
    elapsed: f64,
    clk_tck: f64,
    cores: f64,
) -> Value {
    let Some(previous) = previous else {
        return Value::Null;
    };
    if elapsed <= f64::EPSILON || clk_tck <= 0.0 || cores <= 0.0 {
        return Value::Null;
    }
    let Some(previous_ticks) = previous.get(&key) else {
        return Value::Null;
    };
    let delta_ticks = cpu_ticks.saturating_sub(*previous_ticks) as f64;
    // Share of wall time across all logical cores, so a fully loaded machine is 100%.
    let percent = (delta_ticks / clk_tck) / elapsed / cores * 100.0;
    json!(((percent.clamp(0.0, 100.0)) * 10.0).round() / 10.0)
}

fn attach_tabs(processes: &mut [Value]) {
    let script_stats = collect_script_tab_stats();
    let mut tab_tids: HashSet<(u32, u32)> = HashSet::new();
    let mut tabs_by_pid: HashMap<u32, Vec<Value>> = HashMap::new();
    let fallback_pid = processes
        .iter()
        .find(|process| process["name"] == "Browser")
        .and_then(|process| process["pid"].as_u64())
        .or_else(|| {
            processes
                .first()
                .and_then(|process| process["pid"].as_u64())
        })
        .unwrap_or(0) as u32;

    for tab in embedder_tabs() {
        let matching: Vec<_> = script_stats
            .iter()
            .filter(|stats| stats.webview_id == tab.webview_id)
            .collect();
        let pid = matching
            .first()
            .map(|stats| stats.pid)
            .unwrap_or(fallback_pid);
        let mut memory = 0_u64;
        let mut cpu_sum = 0.0_f64;
        let mut cpu_known = false;
        for stats in &matching {
            tab_tids.insert((stats.pid, stats.tid));
            memory = memory.saturating_add(stats.js_heap_bytes);
            if let Some(cpu) = thread_cpu(processes, stats.pid, stats.tid) {
                cpu_sum += cpu;
                cpu_known = true;
            }
        }
        let name = if tab.title.is_empty() {
            format!("Tab: {}", display_url(&tab.url))
        } else {
            format!("Tab: {}", tab.title)
        };
        tabs_by_pid.entry(pid).or_default().push(json!({
            "name": name,
            "url": tab.url,
            "memory": if matching.is_empty() { Value::Null } else { json!(memory) },
            "cpu": if cpu_known { json!((cpu_sum * 10.0).round() / 10.0) } else { Value::Null },
        }));
    }

    for process in processes.iter_mut() {
        let pid = process["pid"].as_u64().unwrap_or(0) as u32;
        if let Some(mut tabs) = tabs_by_pid.remove(&pid) {
            tabs.sort_by(|a, b| {
                let memory_a = a["memory"].as_u64().unwrap_or(0);
                let memory_b = b["memory"].as_u64().unwrap_or(0);
                memory_b.cmp(&memory_a)
            });
            process["tabs"] = json!(tabs);
        }
        if let Some(threads) = process["threads"].as_array_mut() {
            threads.retain(|thread| {
                let tid = thread["tid"].as_u64().unwrap_or(0) as u32;
                !tab_tids.contains(&(pid, tid))
            });
        }
    }
}

fn thread_cpu(processes: &[Value], pid: u32, tid: u32) -> Option<f64> {
    for process in processes {
        if process["pid"].as_u64()? as u32 != pid {
            continue;
        }
        for thread in process["threads"].as_array()? {
            if thread["tid"].as_u64()? as u32 == tid {
                return thread["cpu"].as_f64();
            }
        }
    }
    None
}

fn display_url(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|parsed| {
            parsed
                .host_str()
                .map(|host| host.to_owned())
                .or_else(|| Some(parsed.path().to_owned()))
        })
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| url.to_owned())
}

fn collect_processes() -> Vec<RawProcess> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        collect_linux_process_tree()
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        collect_fallback()
    }
}

fn collect_fallback() -> Vec<RawProcess> {
    vec![RawProcess {
        pid: std::process::id(),
        ppid: 0,
        name: display_name(true, &[], "Browser"),
        memory: None,
        cpu_ticks: 0,
        threads: vec![],
    }]
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn collect_linux_process_tree() -> Vec<RawProcess> {
    let root_pid = std::process::id();
    let mut by_pid = HashMap::new();
    if let Ok(entries) = std::fs::read_dir("/proc") {
        for entry in entries.flatten() {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<u32>().ok())
            else {
                continue;
            };
            if let Some(process) = read_linux_process(pid, pid == root_pid) {
                by_pid.insert(pid, process);
            }
        }
    }

    if !by_pid.contains_key(&root_pid) {
        return collect_fallback();
    }

    let mut keep = HashSet::from([root_pid]);
    let mut queue = VecDeque::from([root_pid]);
    while let Some(pid) = queue.pop_front() {
        let children: Vec<u32> = by_pid
            .values()
            .filter(|process| process.ppid == pid && process.pid != pid)
            .map(|process| process.pid)
            .collect();
        for child in children {
            if keep.insert(child) {
                queue.push_back(child);
            }
        }
    }

    by_pid
        .into_iter()
        .filter_map(|(pid, process)| keep.contains(&pid).then_some(process))
        .collect()
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn read_linux_process(pid: u32, is_root: bool) -> Option<RawProcess> {
    let stat = parse_stat(&std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)?;
    let cmdline = read_cmdline(pid);
    let memory = read_rss_bytes(pid);
    let mut threads = Vec::new();
    if let Ok(entries) = std::fs::read_dir(format!("/proc/{pid}/task")) {
        for entry in entries.flatten() {
            let Some(tid) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<u32>().ok())
            else {
                continue;
            };
            if tid == pid {
                continue;
            }
            if let Some(thread) = read_linux_thread(pid, tid) {
                threads.push(thread);
            }
        }
    }
    Some(RawProcess {
        pid,
        ppid: stat.ppid,
        name: display_name(is_root, &cmdline, &stat.comm),
        memory,
        cpu_ticks: stat.cpu_ticks,
        threads,
    })
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn read_linux_thread(pid: u32, tid: u32) -> Option<RawThread> {
    let comm = std::fs::read_to_string(format!("/proc/{pid}/task/{tid}/comm"))
        .ok()?
        .trim_end_matches(['\n', '\r'])
        .to_owned();
    let stat = parse_stat(&std::fs::read_to_string(format!("/proc/{pid}/task/{tid}/stat")).ok()?)?;
    Some(RawThread {
        tid,
        name: if comm.is_empty() { stat.comm } else { comm },
        cpu_ticks: stat.cpu_ticks,
    })
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn read_cmdline(pid: u32) -> Vec<String> {
    std::fs::read(format!("/proc/{pid}/cmdline"))
        .unwrap_or_default()
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8_lossy(part).into_owned())
        .collect()
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn read_rss_bytes(pid: u32) -> Option<u64> {
    let contents = std::fs::read_to_string(format!("/proc/{pid}/statm")).ok()?;
    let resident_pages: u64 = contents.split_whitespace().nth(1)?.parse().ok()?;
    Some(resident_pages.saturating_mul(page_size()))
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn page_size() -> u64 {
    let size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if size > 0 { size as u64 } else { 4096 }
}

fn cpu_core_count() -> f64 {
    std::thread::available_parallelism()
        .map(|cores| cores.get() as f64)
        .unwrap_or(1.0)
        .max(1.0)
}

fn clock_ticks_per_second() -> f64 {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        if ticks > 0 { ticks as f64 } else { 100.0 }
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        100.0
    }
}

#[cfg_attr(
    not(any(test, target_os = "linux", target_os = "android")),
    allow(dead_code)
)]
struct StatFields {
    comm: String,
    ppid: u32,
    cpu_ticks: u64,
}

#[cfg_attr(
    not(any(test, target_os = "linux", target_os = "android")),
    allow(dead_code)
)]
fn parse_stat(contents: &str) -> Option<StatFields> {
    let start = contents.find('(')?;
    let end = contents.rfind(')')?;
    if end <= start {
        return None;
    }
    let comm = contents[start + 1..end].to_owned();
    let rest: Vec<&str> = contents[end + 1..].split_whitespace().collect();
    let ppid = rest.get(1)?.parse().ok()?;
    let utime: u64 = rest.get(11)?.parse().ok()?;
    let stime: u64 = rest.get(12)?.parse().ok()?;
    Some(StatFields {
        comm,
        ppid,
        cpu_ticks: utime.saturating_add(stime),
    })
}

fn display_name(is_root: bool, cmdline: &[String], comm: &str) -> String {
    if is_root {
        return "Browser".to_owned();
    }
    if cmdline.iter().any(|arg| arg == "--content-process") {
        return "Content Process".to_owned();
    }
    cmdline
        .first()
        .and_then(|path| std::path::Path::new(path).file_name())
        .and_then(|name| name.to_str())
        .map(|name| name.to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| {
            if comm.is_empty() {
                "Process".to_owned()
            } else {
                comm.to_owned()
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_stat_reads_comm_ppid_and_cpu_ticks() {
        let stat = parse_stat(
            "1234 (aether: Script) S 56 1234 1234 0 -1 0 0 0 0 0 40 10 1 2 20 0 12 0 0 0 0 0",
        )
        .unwrap();
        assert_eq!(stat.comm, "aether: Script");
        assert_eq!(stat.ppid, 56);
        assert_eq!(stat.cpu_ticks, 50);
    }

    #[test]
    fn display_name_uses_browser_and_content_labels() {
        assert_eq!(display_name(true, &[], "aether"), "Browser");
        assert_eq!(
            display_name(
                false,
                &["/usr/bin/aether".to_owned(), "--content-process".to_owned()],
                "aether"
            ),
            "Content Process"
        );
        assert_eq!(
            display_name(false, &["/usr/bin/helper".to_owned()], "helper"),
            "helper"
        );
    }

    #[test]
    fn cpu_percent_divides_by_core_count() {
        let mut previous = HashMap::new();
        previous.insert(SampleKey::Process(1), 0);
        // 100 ticks in 1s at 100 Hz is 1 core-second. With 4 cores that is 25%.
        let value = cpu_percent(Some(&previous), SampleKey::Process(1), 100, 1.0, 100.0, 4.0);
        assert_eq!(value, json!(25.0));
        let fully_loaded =
            cpu_percent(Some(&previous), SampleKey::Process(1), 400, 1.0, 100.0, 4.0);
        assert_eq!(fully_loaded, json!(100.0));
    }

    #[test]
    fn snapshot_json_contains_processes_array() {
        let json: Value = serde_json::from_str(&ProcessSampler::default().snapshot_json()).unwrap();
        assert!(json["processes"].is_array());
        assert!(json["cores"].as_f64().unwrap() >= 1.0);
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[test]
    fn linux_snapshot_includes_current_process() {
        let processes = collect_processes();
        let current = processes
            .iter()
            .find(|process| process.pid == std::process::id())
            .expect("current process should be listed");
        assert_eq!(current.name, "Browser");
        assert!(current.memory.unwrap_or(0) > 0);
    }
}
