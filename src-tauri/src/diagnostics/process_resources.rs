//! Bounded OS snapshots for the desktop monitor. Attribution follows validated
//! process parentage, never executable names or a system-wide resource total.
use serde::Serialize;

const MAX_PROCESSES: usize = 32;

#[derive(Serialize)]
pub(super) struct ProcessReading {
    pid: u32,
    birth: String,
    cpu_ms: f64,
    resident_bytes: Option<u64>,
    disk_write_bytes: Option<u64>,
    io_write_bytes: Option<u64>,
}

#[derive(Serialize)]
pub(super) struct ProcessResources {
    processes: Vec<ProcessReading>,
    bounded: bool,
}

// The numeric start stamp is an OS-specific stable identity within a PID.
// Keep it a string across IPC so JavaScript cannot round a FILETIME.
struct ObservedProcess {
    parent: u32,
    started: u64,
    reading: ProcessReading,
}

fn read_tree(
    mut read: impl FnMut(u32) -> Option<ObservedProcess>,
    mut children: impl FnMut(u32) -> (Vec<u32>, bool),
) -> Option<ProcessResources> {
    let mut processes = vec![read(std::process::id())?];
    let mut bounded = false;
    let mut index = 0;
    while index < processes.len() {
        let parent = processes[index].reading.pid;
        let parent_started = processes[index].started;
        let (candidates, limited) = children(parent);
        bounded |= limited;
        for pid in candidates {
            if processes.iter().any(|known| known.reading.pid == pid) {
                continue;
            }
            if processes.len() == MAX_PROCESSES {
                bounded = true;
                break;
            }
            if let Some(child) = read(pid)
                && child.parent == parent
                && child.started >= parent_started
            {
                processes.push(child);
            }
        }
        index += 1;
    }
    Some(ProcessResources {
        processes: processes.into_iter().map(|value| value.reading).collect(),
        bounded,
    })
}

#[cfg(target_os = "macos")]
pub(super) fn read_resources() -> Option<ProcessResources> {
    fn read(pid: u32) -> Option<ObservedProcess> {
        let mut info = std::mem::MaybeUninit::<libc::rusage_info_v2>::uninit();
        let mut identity = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
        let size = std::mem::size_of::<libc::proc_bsdinfo>() as i32;
        let pid_int = i32::try_from(pid).ok()?;
        // These flavors define the layout and size of each output record.
        let result = unsafe {
            libc::proc_pidinfo(
                pid_int,
                libc::PROC_PIDTBSDINFO,
                0,
                identity.as_mut_ptr().cast(),
                size,
            )
        };
        if result != size {
            return None;
        }
        // proc_pidinfo initialized the full BSD info record.
        let identity = unsafe { identity.assume_init() };
        if unsafe { libc::proc_pid_rusage(pid_int, libc::RUSAGE_INFO_V2, info.as_mut_ptr().cast()) }
            != 0
        {
            return None;
        }
        // proc_pid_rusage initialized the V2 record. Recheck the birth stamp
        // and parent so PID reuse between the independent reads is rejected.
        let info = unsafe { info.assume_init() };
        let mut current = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
        if unsafe {
            libc::proc_pidinfo(
                pid_int,
                libc::PROC_PIDTBSDINFO,
                0,
                current.as_mut_ptr().cast(),
                size,
            )
        } != size
        {
            return None;
        }
        let current = unsafe { current.assume_init() };
        if (
            current.pbi_ppid,
            current.pbi_start_tvsec,
            current.pbi_start_tvusec,
        ) != (
            identity.pbi_ppid,
            identity.pbi_start_tvsec,
            identity.pbi_start_tvusec,
        ) {
            return None;
        }
        let started = info.ri_proc_start_abstime;
        Some(ObservedProcess {
            parent: identity.pbi_ppid,
            started,
            reading: ProcessReading {
                pid,
                birth: started.to_string(),
                cpu_ms: (info.ri_user_time as f64 + info.ri_system_time as f64) / 1_000_000.0,
                resident_bytes: Some(info.ri_resident_size),
                disk_write_bytes: Some(info.ri_diskio_byteswritten),
                io_write_bytes: None,
            },
        })
    }

    read_tree(read, |pid| {
        let mut children = [0i32; MAX_PROCESSES];
        let size = std::mem::size_of_val(&children) as i32;
        // libproc writes at most the supplied buffer size.
        let count =
            unsafe { libc::proc_listchildpids(pid as i32, children.as_mut_ptr().cast(), size) };
        let count = (count.max(0) as usize).min(children.len());
        (
            children[..count]
                .iter()
                .filter_map(|&id| (id > 0).then_some(id as u32))
                .collect(),
            count == children.len(),
        )
    })
}

#[cfg(target_os = "linux")]
fn linux_stat(text: &str) -> Option<(u32, u64, u64, u64)> {
    // comm can contain whitespace or parentheses. All subsequent fields are
    // numeric except the leading state character.
    let mut fields = text.rsplit_once(") ")?.1.split_whitespace();
    let parent = fields.nth(1)?.parse().ok()?;
    let user = fields.nth(9)?.parse::<u64>().ok()?;
    let system = fields.next()?.parse::<u64>().ok()?;
    let started = fields.nth(6)?.parse().ok()?;
    let resident_pages = fields.nth(1)?.parse().ok()?;
    Some((parent, started, user.saturating_add(system), resident_pages))
}

#[cfg(target_os = "linux")]
pub(super) fn read_resources() -> Option<ProcessResources> {
    use std::io::Read;
    // Both selectors are constants supplied by libc.
    let ticks_per_second = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if ticks_per_second <= 0 || page_size <= 0 {
        return None;
    }

    read_tree(
        |pid| {
            let path = format!("/proc/{pid}/stat");
            let (parent, started, ticks, resident_pages) =
                linux_stat(&std::fs::read_to_string(&path).ok()?)?;
            let writes = std::fs::read_to_string(format!("/proc/{pid}/io"))
                .ok()
                .and_then(|text| {
                    text.lines()
                        .find_map(|line| line.strip_prefix("write_bytes:")?.trim().parse().ok())
                });
            let (current_parent, current_start, _, _) =
                linux_stat(&std::fs::read_to_string(path).ok()?)?;
            if current_parent != parent || current_start != started {
                return None;
            }
            Some(ObservedProcess {
                parent,
                started,
                reading: ProcessReading {
                    pid,
                    birth: started.to_string(),
                    cpu_ms: ticks as f64 * 1000.0 / ticks_per_second as f64,
                    resident_bytes: resident_pages.checked_mul(page_size as u64),
                    disk_write_bytes: writes,
                    io_write_bytes: None,
                },
            })
        },
        |pid| {
            const MAX_THREADS: usize = 64;
            let Ok(threads) = std::fs::read_dir(format!("/proc/{pid}/task")) else {
                return (Vec::new(), true);
            };
            let mut children = Vec::new();
            for (index, thread) in threads.enumerate() {
                if index == MAX_THREADS {
                    return (children, true);
                }
                let Ok(thread) = thread else { continue };
                let Ok(file) = std::fs::File::open(thread.path().join("children")) else {
                    if thread.file_name().to_string_lossy() == pid.to_string() {
                        return (children, true);
                    }
                    continue;
                };
                let mut text = String::new();
                if file.take(4096).read_to_string(&mut text).is_err() {
                    continue;
                }
                for id in text
                    .split_whitespace()
                    .filter_map(|id| id.parse::<u32>().ok())
                {
                    if !children.contains(&id) {
                        children.push(id);
                    }
                    if children.len() == MAX_PROCESSES {
                        return (children, true);
                    }
                }
            }
            (children, false)
        },
    )
}

#[cfg(target_os = "windows")]
pub(super) fn read_resources() -> Option<ProcessResources> {
    use windows::Win32::{
        Foundation::{CloseHandle, FILETIME, HANDLE},
        System::{
            Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
                TH32CS_SNAPPROCESS,
            },
            ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS},
            Threading::{
                GetProcessIoCounters, GetProcessTimes, IO_COUNTERS, OpenProcess,
                PROCESS_QUERY_LIMITED_INFORMATION,
            },
        },
    };
    struct Handle(HANDLE);
    impl Drop for Handle {
        fn drop(&mut self) {
            // All instances own a handle returned by a successful Win32 call.
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
    let mut entries = Vec::new();
    let mut bounded = false;
    let owner = std::process::id();
    let before_snapshot = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos()
        / 100
        + 116_444_736_000_000_000;
    if let Ok(snapshot) = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) } {
        let snapshot = Handle(snapshot);
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        // Toolhelp validates dwSize and writes the initialized record.
        let mut result = unsafe { Process32FirstW(snapshot.0, &mut entry) };
        while result.is_ok() {
            if entries.len() >= 8192 {
                bounded = true;
                break;
            }
            entries.push((entry.th32ProcessID, entry.th32ParentProcessID));
            result = unsafe { Process32NextW(snapshot.0, &mut entry) };
        }
    } else {
        bounded = true;
    }

    read_tree(
        |pid| {
            let parent = if pid == owner {
                0
            } else {
                entries.iter().find(|(child, _)| *child == pid)?.1
            };
            // This access mask queries counters only. OpenProcess returns an
            // owned handle that also pins the process identity through the read.
            let handle =
                Handle(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?);
            let mut created = FILETIME::default();
            let mut exited = FILETIME::default();
            let mut kernel = FILETIME::default();
            let mut user = FILETIME::default();
            unsafe { GetProcessTimes(handle.0, &mut created, &mut exited, &mut kernel, &mut user) }
                .ok()?;
            let ticks = |time: FILETIME| {
                (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime)
            };
            let started = ticks(created);
            if ticks(exited) != 0 || u128::from(started) > before_snapshot {
                return None;
            }
            let size = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
            let mut memory = PROCESS_MEMORY_COUNTERS {
                cb: size,
                ..Default::default()
            };
            let resident = unsafe { K32GetProcessMemoryInfo(handle.0, &mut memory, size) }
                .as_bool()
                .then_some(memory.WorkingSetSize as u64);
            let mut io = IO_COUNTERS::default();
            let writes = unsafe { GetProcessIoCounters(handle.0, &mut io) }
                .ok()
                .map(|_| io.WriteTransferCount);
            Some(ObservedProcess {
                parent,
                started,
                reading: ProcessReading {
                    pid,
                    birth: started.to_string(),
                    cpu_ms: (ticks(kernel) as f64 + ticks(user) as f64) / 10_000.0,
                    resident_bytes: resident,
                    disk_write_bytes: None,
                    io_write_bytes: writes,
                },
            })
        },
        |parent| {
            (
                entries
                    .iter()
                    .filter_map(|(pid, ppid)| (parent == *ppid).then_some(*pid))
                    .collect(),
                bounded,
            )
        },
    )
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
pub(super) fn read_resources() -> Option<ProcessResources> {
    None
}

#[cfg(all(
    test,
    any(target_os = "macos", target_os = "linux", target_os = "windows")
))]
mod tests {
    use super::*;

    #[test]
    fn reads_host_and_owned_child() {
        #[cfg(unix)]
        let mut command = {
            let mut command = std::process::Command::new("/bin/sleep");
            command.arg("10");
            command
        };
        #[cfg(windows)]
        let mut command = {
            let mut command = std::process::Command::new("powershell.exe");
            command.args(["-NoProfile", "-Command", "Start-Sleep -Seconds 10"]);
            command
        };
        let mut child = command.spawn().expect("spawn owned process");
        let pid = child.id();
        let result = read_resources();
        let _ = child.kill();
        child.wait().expect("reap owned process");
        let sample = result.expect("read host process");
        let host = &sample.processes[0];
        assert_eq!(host.pid, std::process::id());
        assert!(host.cpu_ms.is_finite() && host.cpu_ms >= 0.0);
        assert!(host.resident_bytes.is_some_and(|bytes| bytes > 0));
        assert!(sample.processes.iter().any(|process| process.pid == pid));
    }
}
