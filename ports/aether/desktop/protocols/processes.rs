/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Snapshot of the browser process tree for `servo:processes`.

#[cfg(any(test, target_os = "linux", target_os = "windows"))]
use std::collections::VecDeque;
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::Instant;

use serde_json::{Value, json};
use servo::profile_traits::tab_stats::{ScriptTabStats, collect_script_tab_stats};

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

        attach_tabs_from(
            &mut json_processes,
            &embedder_tabs(),
            &collect_script_tab_stats(),
        );

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

fn attach_tabs_from(
    processes: &mut [Value],
    tabs: &[EmbedderTab],
    script_stats: &[ScriptTabStats],
) {
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

    for tab in tabs {
        let matching: Vec<_> = script_stats
            .iter()
            .filter(|stats| stats.webview_id == tab.webview_id)
            .collect();
        if matching.is_empty() {
            let name = tab_display_name(tab);
            tabs_by_pid.entry(fallback_pid).or_default().push(json!({
                "name": name,
                "url": tab.url,
                "memory": Value::Null,
                "cpu": Value::Null,
            }));
            continue;
        }

        let mut stats_by_pid: HashMap<u32, Vec<&ScriptTabStats>> = HashMap::new();
        for stats in matching {
            stats_by_pid.entry(stats.pid).or_default().push(stats);
        }
        for (pid, group) in stats_by_pid {
            let mut memory = 0_u64;
            let mut cpu_sum = 0.0_f64;
            let mut cpu_known = false;
            for stats in &group {
                tab_tids.insert((stats.pid, stats.tid));
                memory = memory.saturating_add(stats.js_heap_bytes);
                if let Some(cpu) = thread_cpu(processes, stats.pid, stats.tid) {
                    cpu_sum += cpu;
                    cpu_known = true;
                }
            }
            tabs_by_pid.entry(pid).or_default().push(json!({
                "name": tab_display_name(tab),
                "url": tab.url,
                "memory": json!(memory),
                "cpu": if cpu_known { json!((cpu_sum * 10.0).round() / 10.0) } else { Value::Null },
            }));
        }
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

fn tab_display_name(tab: &EmbedderTab) -> String {
    if tab.title.is_empty() {
        format!("Tab: {}", display_url(&tab.url))
    } else {
        format!("Tab: {}", tab.title)
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
    #[cfg(target_os = "linux")]
    {
        collect_linux_process_tree()
    }
    #[cfg(target_os = "windows")]
    {
        collect_windows_process_tree()
    }
    #[cfg(target_os = "macos")]
    {
        collect_macos_process_tree()
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
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

#[cfg(target_os = "linux")]
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

    keep_descendant_processes(root_pid, by_pid)
}

#[cfg(any(test, target_os = "linux", target_os = "windows"))]
fn descendant_pids(root_pid: u32, parent_by_pid: &HashMap<u32, u32>) -> HashSet<u32> {
    let mut keep = HashSet::from([root_pid]);
    let mut queue = VecDeque::from([root_pid]);
    while let Some(pid) = queue.pop_front() {
        let children: Vec<u32> = parent_by_pid
            .iter()
            .filter_map(|(&child, &ppid)| (ppid == pid && child != pid).then_some(child))
            .collect();
        for child in children {
            if keep.insert(child) {
                queue.push_back(child);
            }
        }
    }
    keep
}

/// PIDs that should get a full process/thread query. `None` means the root is
/// missing from the snapshot and the caller should use [`collect_fallback`].
#[cfg(any(test, target_os = "windows"))]
fn sampled_process_pids(root_pid: u32, parent_by_pid: &HashMap<u32, u32>) -> Option<HashSet<u32>> {
    parent_by_pid
        .contains_key(&root_pid)
        .then(|| descendant_pids(root_pid, parent_by_pid))
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
fn keep_descendant_processes(root_pid: u32, by_pid: HashMap<u32, RawProcess>) -> Vec<RawProcess> {
    let parent_by_pid: HashMap<u32, u32> = by_pid
        .iter()
        .map(|(&pid, process)| (pid, process.ppid))
        .collect();
    let keep = descendant_pids(root_pid, &parent_by_pid);
    by_pid
        .into_iter()
        .filter_map(|(pid, process)| keep.contains(&pid).then_some(process))
        .collect()
}

#[cfg(target_os = "windows")]
fn collect_windows_process_tree() -> Vec<RawProcess> {
    windows_sampler::collect()
}

#[cfg(target_os = "windows")]
mod windows_sampler {
    use std::collections::HashMap;
    use std::mem::size_of;
    use std::path::Path;

    use windows_sys::Win32::Foundation::{
        CloseHandle, FALSE, FILETIME, HANDLE, INVALID_HANDLE_VALUE, LocalFree,
    };
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    use windows_sys::Win32::System::ProcessStatus::{
        K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, GetThreadDescription, GetThreadTimes, OpenProcess, OpenThread,
        PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
        THREAD_QUERY_LIMITED_INFORMATION,
    };

    use super::{RawProcess, RawThread, display_name, keep_descendant_processes};

    struct Handle(HANDLE);

    impl Handle {
        fn new(handle: HANDLE) -> Option<Self> {
            if handle.is_null() || handle == INVALID_HANDLE_VALUE {
                None
            } else {
                Some(Self(handle))
            }
        }
    }

    impl Drop for Handle {
        fn drop(&mut self) {
            // SAFETY: `Handle` is only constructed for valid, owned kernel handles.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    pub(super) fn collect() -> Vec<RawProcess> {
        let root_pid = std::process::id();
        let snapshot = Handle::new(unsafe {
            CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS | TH32CS_SNAPTHREAD, 0)
        });
        let Some(snapshot) = snapshot else {
            return super::collect_fallback();
        };

        let mut entries = HashMap::new();
        let mut process = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        if unsafe { Process32FirstW(snapshot.0, &mut process) } == 0 {
            return super::collect_fallback();
        }
        loop {
            entries.insert(
                process.th32ProcessID,
                (
                    process.th32ParentProcessID,
                    wide_to_string(&process.szExeFile),
                ),
            );
            if unsafe { Process32NextW(snapshot.0, &mut process) } == 0 {
                break;
            }
        }

        let parent_by_pid: HashMap<u32, u32> = entries
            .iter()
            .map(|(&pid, (ppid, _))| (pid, *ppid))
            .collect();
        let Some(keep) = super::sampled_process_pids(root_pid, &parent_by_pid) else {
            return super::collect_fallback();
        };

        let mut threads_by_pid: HashMap<u32, Vec<u32>> = HashMap::new();
        let mut thread = THREADENTRY32 {
            dwSize: size_of::<THREADENTRY32>() as u32,
            ..Default::default()
        };
        if unsafe { Thread32First(snapshot.0, &mut thread) } != 0 {
            loop {
                if keep.contains(&thread.th32OwnerProcessID) {
                    threads_by_pid
                        .entry(thread.th32OwnerProcessID)
                        .or_default()
                        .push(thread.th32ThreadID);
                }
                if unsafe { Thread32Next(snapshot.0, &mut thread) } == 0 {
                    break;
                }
            }
        }

        let root_exe = entries
            .get(&root_pid)
            .map(|(_, exe)| exe.clone())
            .unwrap_or_default();
        let mut by_pid = HashMap::new();
        for pid in keep {
            let Some((ppid, exe)) = entries.get(&pid) else {
                continue;
            };
            by_pid.insert(
                pid,
                read_windows_process(
                    pid,
                    *ppid,
                    exe,
                    &root_exe,
                    pid == root_pid,
                    threads_by_pid.remove(&pid).unwrap_or_default(),
                ),
            );
        }

        keep_descendant_processes(root_pid, by_pid)
    }

    fn read_windows_process(
        pid: u32,
        ppid: u32,
        exe: &str,
        root_exe: &str,
        is_root: bool,
        thread_ids: Vec<u32>,
    ) -> RawProcess {
        let handle =
            Handle::new(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid) });
        let image = handle
            .as_ref()
            .and_then(|handle| query_image_path(handle.0))
            .unwrap_or_else(|| exe.to_owned());
        let same_binary = Path::new(&image).file_name() == Path::new(root_exe).file_name() &&
            Path::new(&image).file_name().is_some();
        let mut cmdline = vec![image.clone()];
        if !is_root && same_binary {
            cmdline.push("--content-process".to_owned());
        }
        let comm = Path::new(&image)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(exe);
        let (memory, cpu_ticks) = handle
            .as_ref()
            .map(|handle| {
                (
                    process_working_set(handle.0),
                    process_cpu_ticks(handle.0).unwrap_or(0),
                )
            })
            .unwrap_or((None, 0));

        let threads = thread_ids
            .into_iter()
            .filter_map(read_windows_thread)
            .collect();

        RawProcess {
            pid,
            ppid,
            name: display_name(is_root, &cmdline, comm),
            memory,
            cpu_ticks,
            threads,
        }
    }

    fn read_windows_thread(tid: u32) -> Option<RawThread> {
        let handle =
            Handle::new(unsafe { OpenThread(THREAD_QUERY_LIMITED_INFORMATION, FALSE, tid) })?;
        Some(RawThread {
            tid,
            name: thread_name(handle.0).unwrap_or_else(|| format!("Thread {tid}")),
            cpu_ticks: process_cpu_ticks_for_thread(handle.0).unwrap_or(0),
        })
    }

    fn process_working_set(handle: HANDLE) -> Option<u64> {
        let mut counters = PROCESS_MEMORY_COUNTERS {
            cb: size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
            ..Default::default()
        };
        (unsafe { K32GetProcessMemoryInfo(handle, &mut counters, counters.cb) } != 0)
            .then_some(counters.WorkingSetSize as u64)
    }

    fn process_cpu_ticks(handle: HANDLE) -> Option<u64> {
        let mut creation = FILETIME::default();
        let mut exit = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        (unsafe { GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) } != 0)
            .then_some(filetime_ticks(kernel).saturating_add(filetime_ticks(user)))
    }

    fn process_cpu_ticks_for_thread(handle: HANDLE) -> Option<u64> {
        let mut creation = FILETIME::default();
        let mut exit = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        (unsafe { GetThreadTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) } != 0)
            .then_some(filetime_ticks(kernel).saturating_add(filetime_ticks(user)))
    }

    fn query_image_path(handle: HANDLE) -> Option<String> {
        let mut buf = [0u16; 1024];
        let mut size = buf.len() as u32;
        (unsafe { QueryFullProcessImageNameW(handle, 0, buf.as_mut_ptr(), &mut size) } != 0)
            .then(|| String::from_utf16_lossy(&buf[..size as usize]))
    }

    fn thread_name(handle: HANDLE) -> Option<String> {
        let mut description = std::ptr::null_mut();
        let status = unsafe { GetThreadDescription(handle, &mut description) };
        if status < 0 || description.is_null() {
            return None;
        }
        let name = unsafe {
            let mut len = 0;
            while *description.add(len) != 0 {
                len += 1;
            }
            String::from_utf16_lossy(std::slice::from_raw_parts(description, len))
        };
        unsafe {
            LocalFree(description.cast());
        }
        let name = name.trim();
        (!name.is_empty()).then(|| name.to_owned())
    }

    fn filetime_ticks(time: FILETIME) -> u64 {
        ((time.dwHighDateTime as u64) << 32) | u64::from(time.dwLowDateTime)
    }

    fn wide_to_string(buf: &[u16]) -> String {
        let len = buf.iter().position(|&unit| unit == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..len])
    }
}

#[cfg(target_os = "macos")]
fn collect_macos_process_tree() -> Vec<RawProcess> {
    macos_sampler::collect()
}

#[cfg(target_os = "macos")]
mod macos_sampler {
    use std::collections::{HashMap, HashSet, VecDeque};
    use std::mem::{MaybeUninit, size_of};
    use std::path::Path;
    use std::ptr;

    use super::{RawProcess, RawThread, display_name, parse_procargs2};

    const PROC_PIDLISTTHREADS: libc::c_int = 6;

    pub(super) fn collect() -> Vec<RawProcess> {
        let root_pid = std::process::id();
        let Some(root) = read_macos_process(root_pid, true, None) else {
            return super::collect_fallback();
        };
        let root_exe = process_image_path(root_pid).unwrap_or_default();
        let mut by_pid = HashMap::new();
        by_pid.insert(root_pid, root);
        for pid in descendant_pids(root_pid) {
            if pid == root_pid {
                continue;
            }
            if let Some(process) = read_macos_process(pid, false, Some(&root_exe)) {
                by_pid.insert(pid, process);
            }
        }
        by_pid.into_values().collect()
    }

    fn descendant_pids(root_pid: u32) -> HashSet<u32> {
        let mut keep = HashSet::from([root_pid]);
        let mut queue = VecDeque::from([root_pid]);
        while let Some(pid) = queue.pop_front() {
            for child in list_child_pids(pid) {
                if keep.insert(child) {
                    queue.push_back(child);
                }
            }
        }
        keep
    }

    fn list_child_pids(ppid: u32) -> Vec<u32> {
        let needed = unsafe { libc::proc_listchildpids(ppid as libc::pid_t, ptr::null_mut(), 0) };
        if needed <= 0 {
            return Vec::new();
        }
        let count = (needed as usize / size_of::<libc::pid_t>()).saturating_add(8);
        let mut buf = vec![0 as libc::pid_t; count];
        let written = unsafe {
            libc::proc_listchildpids(
                ppid as libc::pid_t,
                buf.as_mut_ptr().cast(),
                (buf.len() * size_of::<libc::pid_t>()) as libc::c_int,
            )
        };
        if written <= 0 {
            return Vec::new();
        }
        buf.into_iter()
            .take(written as usize / size_of::<libc::pid_t>())
            .filter_map(|pid| (pid > 0).then_some(pid as u32))
            .collect()
    }

    fn read_macos_process(pid: u32, is_root: bool, root_exe: Option<&str>) -> Option<RawProcess> {
        let info = proc_taskallinfo(pid)?;
        let image = process_image_path(pid).unwrap_or_else(|| process_comm(&info));
        let mut cmdline = read_macos_cmdline(pid);
        if cmdline.is_empty() {
            cmdline.push(image.clone());
            if !is_root &&
                let Some(root_exe) = root_exe &&
                Path::new(&image).file_name().is_some() &&
                Path::new(&image).file_name() == Path::new(root_exe).file_name()
            {
                cmdline.push("--content-process".to_owned());
            }
        }
        let comm = Path::new(&image)
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| name.to_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| process_comm(&info));
        Some(RawProcess {
            pid,
            ppid: info.pbsd.pbi_ppid,
            name: display_name(is_root, &cmdline, &comm),
            memory: Some(info.ptinfo.pti_resident_size),
            cpu_ticks: info
                .ptinfo
                .pti_total_user
                .saturating_add(info.ptinfo.pti_total_system),
            threads: list_thread_ids(pid)
                .into_iter()
                .filter(|&tid| tid != u64::from(pid))
                .filter_map(|thread_id| read_macos_thread(pid, thread_id))
                .collect(),
        })
    }

    fn read_macos_thread(pid: u32, thread_id: u64) -> Option<RawThread> {
        let info = proc_threadinfo(pid, thread_id)?;
        let name = c_chars_to_string(&info.pth_name);
        Some(RawThread {
            tid: thread_id as u32,
            name: if name.is_empty() {
                format!("Thread {thread_id}")
            } else {
                name
            },
            cpu_ticks: info.pth_user_time.saturating_add(info.pth_system_time),
        })
    }

    fn list_thread_ids(pid: u32) -> Vec<u64> {
        let needed = unsafe {
            libc::proc_pidinfo(
                pid as libc::c_int,
                PROC_PIDLISTTHREADS,
                0,
                ptr::null_mut(),
                0,
            )
        };
        if needed <= 0 {
            return Vec::new();
        }
        let count = (needed as usize / size_of::<u64>()).saturating_add(8);
        let mut buf = vec![0u64; count];
        let written = unsafe {
            libc::proc_pidinfo(
                pid as libc::c_int,
                PROC_PIDLISTTHREADS,
                0,
                buf.as_mut_ptr().cast(),
                (buf.len() * size_of::<u64>()) as libc::c_int,
            )
        };
        if written <= 0 {
            return Vec::new();
        }
        buf.into_iter()
            .take(written as usize / size_of::<u64>())
            .filter(|tid| *tid != 0)
            .collect()
    }

    fn proc_taskallinfo(pid: u32) -> Option<libc::proc_taskallinfo> {
        let mut info = MaybeUninit::<libc::proc_taskallinfo>::uninit();
        let size = size_of::<libc::proc_taskallinfo>() as libc::c_int;
        let written = unsafe {
            libc::proc_pidinfo(
                pid as libc::c_int,
                libc::PROC_PIDTASKALLINFO,
                0,
                info.as_mut_ptr().cast(),
                size,
            )
        };
        (written == size).then(|| unsafe { info.assume_init() })
    }

    fn proc_threadinfo(pid: u32, thread_id: u64) -> Option<libc::proc_threadinfo> {
        let mut info = MaybeUninit::<libc::proc_threadinfo>::uninit();
        let size = size_of::<libc::proc_threadinfo>() as libc::c_int;
        let written = unsafe {
            libc::proc_pidinfo(
                pid as libc::c_int,
                libc::PROC_PIDTHREADINFO,
                thread_id,
                info.as_mut_ptr().cast(),
                size,
            )
        };
        (written == size).then(|| unsafe { info.assume_init() })
    }

    fn process_image_path(pid: u32) -> Option<String> {
        let mut buf = [0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
        let written = unsafe {
            libc::proc_pidpath(
                pid as libc::c_int,
                buf.as_mut_ptr().cast(),
                buf.len() as u32,
            )
        };
        (written > 0).then(|| String::from_utf8_lossy(&buf[..written as usize]).into_owned())
    }

    fn process_comm(info: &libc::proc_taskallinfo) -> String {
        let name = c_chars_to_string(&info.pbsd.pbi_name);
        if name.is_empty() {
            c_chars_to_string(&info.pbsd.pbi_comm)
        } else {
            name
        }
    }

    fn read_macos_cmdline(pid: u32) -> Vec<String> {
        let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as libc::c_int];
        let mut size = 0usize;
        let status = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                mib.len() as libc::c_uint,
                ptr::null_mut(),
                &mut size,
                ptr::null_mut(),
                0,
            )
        };
        if status != 0 || size < 4 {
            return Vec::new();
        }
        let mut buf = vec![0u8; size];
        let status = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                mib.len() as libc::c_uint,
                buf.as_mut_ptr().cast(),
                &mut size,
                ptr::null_mut(),
                0,
            )
        };
        if status != 0 || size < 4 {
            return Vec::new();
        }
        buf.truncate(size);
        parse_procargs2(&buf)
    }

    fn c_chars_to_string(buf: &[libc::c_char]) -> String {
        let bytes = unsafe { std::slice::from_raw_parts(buf.as_ptr().cast::<u8>(), buf.len()) };
        let end = bytes
            .iter()
            .position(|&byte| byte == 0)
            .unwrap_or(bytes.len());
        String::from_utf8_lossy(&bytes[..end]).into_owned()
    }
}

#[cfg(target_os = "linux")]
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

#[cfg(target_os = "linux")]
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

#[cfg(target_os = "linux")]
fn read_cmdline(pid: u32) -> Vec<String> {
    std::fs::read(format!("/proc/{pid}/cmdline"))
        .unwrap_or_default()
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8_lossy(part).into_owned())
        .collect()
}

#[cfg(target_os = "linux")]
fn read_rss_bytes(pid: u32) -> Option<u64> {
    let contents = std::fs::read_to_string(format!("/proc/{pid}/statm")).ok()?;
    let resident_pages: u64 = contents.split_whitespace().nth(1)?.parse().ok()?;
    Some(resident_pages.saturating_mul(page_size()))
}

#[cfg(target_os = "linux")]
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
    #[cfg(target_os = "linux")]
    {
        let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        if ticks > 0 { ticks as f64 } else { 100.0 }
    }
    #[cfg(target_os = "windows")]
    {
        // GetProcessTimes / GetThreadTimes use 100-nanosecond FILETIME units.
        10_000_000.0
    }
    #[cfg(target_os = "macos")]
    {
        // libproc task/thread times are nanoseconds.
        1_000_000_000.0
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        100.0
    }
}

#[cfg_attr(not(any(test, target_os = "linux")), allow(dead_code))]
struct StatFields {
    comm: String,
    ppid: u32,
    cpu_ticks: u64,
}

#[cfg_attr(not(any(test, target_os = "linux")), allow(dead_code))]
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

#[cfg_attr(not(any(test, target_os = "macos")), allow(dead_code))]
fn parse_procargs2(buf: &[u8]) -> Vec<String> {
    if buf.len() < 4 {
        return Vec::new();
    }
    let argc = i32::from_ne_bytes(buf[0..4].try_into().unwrap_or_default());
    if argc <= 0 {
        return Vec::new();
    }
    let mut pos = 4;
    while pos < buf.len() && buf[pos] != 0 {
        pos += 1;
    }
    pos += 1;
    while pos < buf.len() && buf[pos] == 0 {
        pos += 1;
    }
    let mut args = Vec::with_capacity(argc as usize);
    for _ in 0..argc {
        if pos >= buf.len() {
            break;
        }
        let start = pos;
        while pos < buf.len() && buf[pos] != 0 {
            pos += 1;
        }
        if start < pos {
            args.push(String::from_utf8_lossy(&buf[start..pos]).into_owned());
        }
        pos += 1;
    }
    args
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

    fn previous_sample(key: SampleKey, ticks: u64) -> HashMap<SampleKey, u64> {
        let mut previous = HashMap::new();
        previous.insert(key, ticks);
        previous
    }

    fn process_json(pid: u32, name: &str, memory: u64, threads: Vec<Value>) -> Value {
        json!({
            "pid": pid,
            "name": name,
            "memory": memory,
            "cpu": 1.0,
            "threads": threads,
            "tabs": [],
        })
    }

    fn thread_json(tid: u32, name: &str, cpu: f64) -> Value {
        json!({
            "tid": tid,
            "name": name,
            "cpu": cpu,
        })
    }

    fn tab_stats(pid: u32, tid: u32, webview_id: &str, js_heap_bytes: u64) -> ScriptTabStats {
        ScriptTabStats {
            pid,
            tid,
            webview_id: webview_id.to_owned(),
            url: "https://example.com/".to_owned(),
            js_heap_bytes,
            updated_ms: 1,
        }
    }

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
    fn parse_stat_keeps_parentheses_inside_comm() {
        let stat = parse_stat("9 (foo (bar) baz) S 1 9 9 0 -1 0 0 0 0 0 3 4 0 0 20 0 1 0 0 0 0 0")
            .unwrap();
        assert_eq!(stat.comm, "foo (bar) baz");
        assert_eq!(stat.ppid, 1);
        assert_eq!(stat.cpu_ticks, 7);
    }

    #[test]
    fn parse_stat_rejects_truncated_input() {
        assert!(parse_stat("").is_none());
        assert!(parse_stat("1234 missing-comm").is_none());
        assert!(parse_stat("1234 (name) S 1").is_none());
    }

    #[test]
    fn display_name_uses_browser_and_content_labels() {
        assert_eq!(display_name(true, &[], "aether"), "Browser");
        assert_eq!(
            display_name(
                true,
                &["/usr/bin/aether".to_owned(), "--content-process".to_owned()],
                "aether"
            ),
            "Browser"
        );
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
        assert_eq!(display_name(false, &[], "gpu-process"), "gpu-process");
        assert_eq!(display_name(false, &[], ""), "Process");
    }

    #[test]
    fn parse_procargs2_reads_argc_and_argv() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&3i32.to_ne_bytes());
        buf.extend_from_slice(b"/usr/bin/aether\0\0\0");
        buf.extend_from_slice(b"/usr/bin/aether\0--content-process\0ipc-name\0");
        assert_eq!(
            parse_procargs2(&buf),
            vec![
                "/usr/bin/aether".to_owned(),
                "--content-process".to_owned(),
                "ipc-name".to_owned()
            ]
        );
        assert!(parse_procargs2(&[]).is_empty());
        assert!(parse_procargs2(&0i32.to_ne_bytes()).is_empty());
    }

    #[test]
    fn display_url_prefers_host_then_path() {
        assert_eq!(display_url("https://example.com/path?q=1"), "example.com");
        assert_eq!(display_url("file:///tmp/page.html"), "/tmp/page.html");
        assert_eq!(display_url("servo:processes"), "processes");
        assert_eq!(display_url("not a url"), "not a url");
    }

    #[test]
    fn cpu_percent_divides_by_core_count() {
        let previous = previous_sample(SampleKey::Process(1), 0);
        // 100 ticks in 1s at 100 Hz is 1 core-second. With 4 cores that is 25%.
        let value = cpu_percent(Some(&previous), SampleKey::Process(1), 100, 1.0, 100.0, 4.0);
        assert_eq!(value, json!(25.0));
        let fully_loaded =
            cpu_percent(Some(&previous), SampleKey::Process(1), 400, 1.0, 100.0, 4.0);
        assert_eq!(fully_loaded, json!(100.0));
    }

    #[test]
    fn cpu_percent_is_null_without_a_previous_sample() {
        assert_eq!(
            cpu_percent(None, SampleKey::Process(1), 100, 1.0, 100.0, 4.0),
            Value::Null
        );
        let previous = previous_sample(SampleKey::Process(2), 0);
        assert_eq!(
            cpu_percent(Some(&previous), SampleKey::Process(1), 100, 1.0, 100.0, 4.0),
            Value::Null
        );
        let previous = previous_sample(SampleKey::Process(1), 0);
        assert_eq!(
            cpu_percent(Some(&previous), SampleKey::Process(1), 100, 0.0, 100.0, 4.0),
            Value::Null
        );
    }

    #[test]
    fn cpu_percent_clamps_and_rounds() {
        let previous = previous_sample(SampleKey::Process(1), 0);
        assert_eq!(
            cpu_percent(Some(&previous), SampleKey::Process(1), 800, 1.0, 100.0, 4.0),
            json!(100.0)
        );
        // 13 ticks / 100 Hz / 4 cores = 3.25% -> 3.3.
        assert_eq!(
            cpu_percent(Some(&previous), SampleKey::Process(1), 13, 1.0, 100.0, 4.0),
            json!(3.3)
        );
        let decreasing = previous_sample(SampleKey::Process(1), 50);
        assert_eq!(
            cpu_percent(
                Some(&decreasing),
                SampleKey::Process(1),
                10,
                1.0,
                100.0,
                4.0
            ),
            json!(0.0)
        );
    }

    #[test]
    fn attach_tabs_uses_title_or_url_and_falls_back_to_browser() {
        let mut processes = vec![
            process_json(10, "Browser", 100, vec![]),
            process_json(20, "Content Process", 50, vec![]),
        ];
        let tabs = vec![
            EmbedderTab {
                webview_id: "tab-1".to_owned(),
                title: "Example".to_owned(),
                url: "https://example.com/page".to_owned(),
            },
            EmbedderTab {
                webview_id: "tab-2".to_owned(),
                title: String::new(),
                url: "https://docs.rs/aether".to_owned(),
            },
        ];
        attach_tabs_from(&mut processes, &tabs, &[]);

        assert_eq!(processes[0]["tabs"][0]["name"], "Tab: Example");
        assert_eq!(processes[0]["tabs"][0]["url"], "https://example.com/page");
        assert!(processes[0]["tabs"][0]["memory"].is_null());
        assert!(processes[0]["tabs"][0]["cpu"].is_null());
        assert_eq!(processes[0]["tabs"][1]["name"], "Tab: docs.rs");
        assert!(processes[1]["tabs"].as_array().unwrap().is_empty());
    }

    #[test]
    fn attach_tabs_matches_script_thread_and_hides_it() {
        let mut processes = vec![process_json(
            20,
            "Content Process",
            50,
            vec![thread_json(7, "Script", 4.2), thread_json(8, "Layout", 1.0)],
        )];
        let tabs = vec![EmbedderTab {
            webview_id: "wv-1".to_owned(),
            title: "News".to_owned(),
            url: "https://news.example/".to_owned(),
        }];
        attach_tabs_from(&mut processes, &tabs, &[tab_stats(20, 7, "wv-1", 1024)]);

        assert_eq!(processes[0]["tabs"].as_array().unwrap().len(), 1);
        assert_eq!(processes[0]["tabs"][0]["memory"], 1024);
        assert_eq!(processes[0]["tabs"][0]["cpu"], 4.2);
        let threads = processes[0]["threads"].as_array().unwrap();
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0]["name"], "Layout");
    }

    #[test]
    fn attach_tabs_sorts_by_memory_and_sums_cpu_across_threads() {
        let mut processes = vec![process_json(
            20,
            "Content Process",
            50,
            vec![
                thread_json(1, "Script A", 1.25),
                thread_json(2, "Script B", 2.25),
            ],
        )];
        let tabs = vec![
            EmbedderTab {
                webview_id: "small".to_owned(),
                title: "Small".to_owned(),
                url: "https://small.test/".to_owned(),
            },
            EmbedderTab {
                webview_id: "large".to_owned(),
                title: "Large".to_owned(),
                url: "https://large.test/".to_owned(),
            },
        ];
        let stats = vec![
            tab_stats(20, 1, "small", 100),
            tab_stats(20, 2, "large", 500),
        ];
        attach_tabs_from(&mut processes, &tabs, &stats);

        let attached = processes[0]["tabs"].as_array().unwrap();
        assert_eq!(attached[0]["name"], "Tab: Large");
        assert_eq!(attached[0]["memory"], 500);
        assert_eq!(attached[0]["cpu"], 2.3);
        assert_eq!(attached[1]["name"], "Tab: Small");
        assert_eq!(attached[1]["cpu"], 1.3);
        assert!(processes[0]["threads"].as_array().unwrap().is_empty());
    }

    #[test]
    fn attach_tabs_keeps_one_entry_per_process_for_the_same_webview() {
        let mut processes = vec![
            process_json(
                20,
                "Content Process",
                50,
                vec![thread_json(1, "Script A", 1.0)],
            ),
            process_json(
                21,
                "Content Process",
                40,
                vec![thread_json(2, "Script B", 2.0)],
            ),
        ];
        let tabs = vec![EmbedderTab {
            webview_id: "shared".to_owned(),
            title: "Shared".to_owned(),
            url: "https://shared.test/".to_owned(),
        }];
        let stats = vec![
            tab_stats(20, 1, "shared", 100),
            tab_stats(21, 2, "shared", 250),
        ];
        attach_tabs_from(&mut processes, &tabs, &stats);

        assert_eq!(processes[0]["tabs"].as_array().unwrap().len(), 1);
        assert_eq!(processes[0]["tabs"][0]["memory"], 100);
        assert_eq!(processes[0]["tabs"][0]["cpu"], 1.0);
        assert_eq!(processes[1]["tabs"].as_array().unwrap().len(), 1);
        assert_eq!(processes[1]["tabs"][0]["memory"], 250);
        assert_eq!(processes[1]["tabs"][0]["cpu"], 2.0);
        assert!(processes[0]["threads"].as_array().unwrap().is_empty());
        assert!(processes[1]["threads"].as_array().unwrap().is_empty());
    }

    #[test]
    fn snapshot_json_contains_sorted_process_schema() {
        let json: Value = serde_json::from_str(&ProcessSampler::default().snapshot_json()).unwrap();
        assert!(json["cores"].as_f64().unwrap() >= 1.0);
        let processes = json["processes"].as_array().expect("processes array");
        assert!(!processes.is_empty());

        let mut previous_memory = u64::MAX;
        for process in processes {
            assert!(process["pid"].as_u64().is_some());
            assert!(process["name"].as_str().is_some());
            assert!(process.get("memory").is_some());
            assert!(process.get("cpu").is_some());
            assert!(process["cpu"].is_null(), "first sample has no CPU delta");
            assert!(process["threads"].is_array());
            assert!(process["tabs"].is_array());
            assert!(process.get("memory_details").is_none());
            let memory = process["memory"].as_u64().unwrap_or(0);
            assert!(memory <= previous_memory);
            previous_memory = memory;
        }
    }

    fn parent_map(pairs: &[(u32, u32)]) -> HashMap<u32, u32> {
        pairs.iter().copied().collect()
    }

    #[cfg(any(target_os = "linux", target_os = "windows"))]
    fn raw_process(pid: u32, ppid: u32) -> RawProcess {
        RawProcess {
            pid,
            ppid,
            name: format!("proc-{pid}"),
            memory: None,
            cpu_ticks: 0,
            threads: vec![],
        }
    }

    #[test]
    fn descendant_pids_keeps_nested_children_only() {
        let parents = parent_map(&[(10, 1), (11, 10), (12, 10), (13, 11), (99, 1), (0, 0)]);
        assert_eq!(
            descendant_pids(10, &parents),
            HashSet::from([10, 11, 12, 13])
        );
    }

    #[test]
    fn descendant_pids_ignores_self_parent_cycles() {
        let parents = parent_map(&[(10, 10), (11, 10)]);
        assert_eq!(descendant_pids(10, &parents), HashSet::from([10, 11]));
    }

    #[test]
    fn sampled_process_pids_falls_back_when_root_is_absent() {
        let parents = parent_map(&[(11, 10), (12, 1), (99, 1)]);
        assert!(sampled_process_pids(10, &parents).is_none());
    }

    #[test]
    fn sampled_process_pids_returns_only_descendants() {
        let parents = parent_map(&[(10, 1), (11, 10), (12, 11), (99, 1), (0, 0)]);
        assert_eq!(
            sampled_process_pids(10, &parents).unwrap(),
            HashSet::from([10, 11, 12])
        );
    }

    #[cfg(any(target_os = "linux", target_os = "windows"))]
    #[test]
    fn keep_descendant_processes_drops_unrelated_pids() {
        let mut by_pid = HashMap::new();
        for (pid, ppid) in [(10, 1), (11, 10), (99, 1)] {
            by_pid.insert(pid, raw_process(pid, ppid));
        }
        let mut pids: Vec<u32> = keep_descendant_processes(10, by_pid)
            .iter()
            .map(|process| process.pid)
            .collect();
        pids.sort_unstable();
        assert_eq!(pids, vec![10, 11]);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_snapshot_includes_current_process_tree_only() {
        let processes = collect_processes();
        let root_pid = std::process::id();
        let current = processes
            .iter()
            .find(|process| process.pid == root_pid)
            .expect("current process should be listed");
        assert_eq!(current.name, "Browser");
        assert!(current.memory.unwrap_or(0) > 0);
        assert!(
            processes
                .iter()
                .all(|process| process.pid != 1 || root_pid == 1),
            "unrelated init process should not appear in the tree"
        );

        let self_stat = parse_stat(&std::fs::read_to_string("/proc/self/stat").unwrap()).unwrap();
        assert!(!self_stat.comm.is_empty());
        assert!(current.threads.iter().all(|thread| thread.tid != root_pid));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_snapshot_includes_current_process_tree_only() {
        let processes = collect_processes();
        let root_pid = std::process::id();
        let current = processes
            .iter()
            .find(|process| process.pid == root_pid)
            .expect("current process should be listed");
        assert_eq!(current.name, "Browser");
        assert!(current.memory.unwrap_or(0) > 0);
        assert!(
            processes.iter().all(|process| process.pid != 0),
            "the idle process should not appear in the tree"
        );
        assert!(
            current.cpu_ticks > 0,
            "the current process should have consumed some CPU time"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_snapshot_includes_current_process_tree_only() {
        let processes = collect_processes();
        let root_pid = std::process::id();
        let current = processes
            .iter()
            .find(|process| process.pid == root_pid)
            .expect("current process should be listed");
        assert_eq!(current.name, "Browser");
        assert!(current.memory.unwrap_or(0) > 0);
        assert!(
            processes.iter().all(|process| process.pid != 0),
            "kernel_task should not appear in the tree"
        );
        assert!(
            current.cpu_ticks > 0,
            "the current process should have consumed some CPU time"
        );
        assert!(current.threads.iter().all(|thread| thread.tid != root_pid));
    }
}
