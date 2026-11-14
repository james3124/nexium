//! Process and system memory usage reporting.

use serde::Serialize;
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct ProcessMemory {
    pub rss_kb: u64,
    pub vsz_kb: u64,
    pub threads: u64,
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct SystemMemory {
    pub total_kb: u64,
    pub available_kb: u64,
}

fn proc_status_value(text: &str, key: &str) -> Option<u64> {
    text.lines()
        .find_map(|line| line.strip_prefix(key))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|value| value.parse::<u64>().ok())
}

/// Resident/virtual memory and thread count for this process (Linux/Android).
pub fn process_memory() -> ProcessMemory {
    let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
        return ProcessMemory::default();
    };
    ProcessMemory {
        rss_kb: proc_status_value(&status, "VmRSS:").unwrap_or(0),
        vsz_kb: proc_status_value(&status, "VmSize:").unwrap_or(0),
        threads: proc_status_value(&status, "Threads:").unwrap_or(0),
    }
}

pub fn system_memory() -> Option<SystemMemory> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    Some(SystemMemory {
        total_kb: proc_status_value(&text, "MemTotal:")?,
        available_kb: proc_status_value(&text, "MemAvailable:").unwrap_or(0),
    })
}

/// Process + system usage as JSON for health/status endpoints.
pub fn usage_json() -> Value {
    json!({
        "process": process_memory(),
        "system": system_memory(),
    })
}
