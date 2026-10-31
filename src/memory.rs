//! On-demand owner facts, not a heap estimator or periodic sampler.
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

/// One response carried through the existing UI notification channels.
#[derive(Clone, Debug)]
pub struct UiMemoryReply(Arc<Mutex<Option<tokio::sync::oneshot::Sender<Value>>>>);
impl UiMemoryReply {
    pub(crate) fn new() -> (Self, tokio::sync::oneshot::Receiver<Value>) {
        let (send, receive) = tokio::sync::oneshot::channel();
        (Self(Arc::new(Mutex::new(Some(send)))), receive)
    }
    pub(crate) fn is_closed(&self) -> bool {
        self.0
            .lock()
            .expect("memory reply lock")
            .as_ref()
            .is_none_or(|s| s.is_closed())
    }
    pub(crate) fn finish(&self, value: Value) {
        if let Some(send) = self.0.lock().expect("memory reply lock").take() {
            let _ = send.send(value);
        }
    }
}
pub(crate) fn cache(bytes: usize, limit: u64, entries: usize, unit: &str) -> Value {
    json!({"retained_bytes":bytes,"limit_bytes":limit,"entries":entries,"entry_unit":unit,"unavailable_reason":null})
}
pub(crate) fn unknown_cache(limit: u64) -> Value {
    json!({"retained_bytes":null,"limit_bytes":limit,"entries":null,"unavailable_reason":"UI cache owner is not available"})
}
fn metric(bytes: Option<u64>, reason: Option<String>) -> Value {
    json!({"bytes":bytes,"unavailable_reason":reason})
}
pub(crate) fn process_snapshot() -> Value {
    let mut metrics = json!({});
    for key in ["footprint", "rss", "pss", "working_set", "private_bytes"] {
        metrics[key] = metric(None, Some("Not provided on this platform".into()));
    }
    native(&mut metrics);
    json!({"pid":std::process::id(),"platform":std::env::consts::OS,"observed_at":chrono::Utc::now().to_rfc3339(),"metrics":metrics})
}
#[cfg(target_os = "linux")]
fn native(metrics: &mut Value) {
    use std::io::Read;
    fn read(path: &str) -> Result<String, String> {
        let mut text = String::new();
        std::fs::File::open(path)
            .map_err(|e| e.to_string())?
            .take(65_537)
            .read_to_string(&mut text)
            .map_err(|e| e.to_string())?;
        if text.len() > 65_536 {
            return Err("Process metric file exceeds read limit".into());
        }
        Ok(text)
    }
    let status = read("/proc/self/status");
    let rollup = read("/proc/self/smaps_rollup");
    for (name, input, key) in [("rss", &status, "VmRSS:"), ("pss", &rollup, "Pss:")] {
        let result = input.as_ref().map_err(Clone::clone).and_then(|text| {
            proc_kib(text, key).ok_or_else(|| "Metric absent or invalid in proc data".into())
        });
        metrics[name] = match result {
            Ok(value) => metric(Some(value), None),
            Err(reason) => metric(None, Some(reason)),
        };
    }
}
#[cfg(any(target_os = "linux", test))]
fn proc_kib(text: &str, key: &str) -> Option<u64> {
    let mut parts = text
        .lines()
        .find_map(|line| line.strip_prefix(key))?
        .split_whitespace();
    let value: u64 = parts.next()?.parse().ok()?;
    (parts.next()? == "kB" && parts.next().is_none())
        .then_some(value)?
        .checked_mul(1024)
}
#[cfg(target_os = "macos")]
fn native(metrics: &mut Value) {
    // libc's flavor-specific ABI supplies bytes; no conversion or RSS inference.
    let mut info = unsafe { std::mem::zeroed::<libc::rusage_info_v2>() };
    let result = unsafe {
        libc::proc_pid_rusage(
            libc::getpid(),
            libc::RUSAGE_INFO_V2,
            std::ptr::addr_of_mut!(info).cast(),
        )
    };
    if result == 0 {
        metrics["footprint"] = metric(Some(info.ri_phys_footprint), None);
        metrics["rss"] = metric(Some(info.ri_resident_size), None);
    } else {
        let reason = std::io::Error::last_os_error().to_string();
        for key in ["footprint", "rss"] {
            metrics[key] = metric(None, Some(reason.clone()));
        }
    }
}
#[cfg(windows)]
fn native(metrics: &mut Value) {
    use windows_sys::Win32::System::{
        ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX},
        Threading::GetCurrentProcess,
    };
    let mut info = unsafe { std::mem::zeroed::<PROCESS_MEMORY_COUNTERS_EX>() };
    info.cb = std::mem::size_of_val(&info) as u32;
    let result = unsafe {
        K32GetProcessMemoryInfo(
            GetCurrentProcess(),
            std::ptr::addr_of_mut!(info).cast(),
            info.cb,
        )
    };
    if result != 0 {
        metrics["working_set"] = metric(Some(info.WorkingSetSize as u64), None);
        metrics["private_bytes"] = metric(Some(info.PrivateUsage as u64), None);
    } else {
        let reason = std::io::Error::last_os_error().to_string();
        for key in ["working_set", "private_bytes"] {
            metrics[key] = metric(None, Some(reason.clone()));
        }
    }
}
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn native(_: &mut Value) {}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn proc_units_unknowns_and_native_fields_are_factual() {
        assert_eq!(proc_kib("VmRSS:  23 kB\n", "VmRSS:"), Some(23 * 1024));
        for text in [
            "Pss: 1 MB",
            "Pss: unknown kB",
            "Pss: 18446744073709551615 kB",
            "no value",
        ] {
            assert_eq!(proc_kib(text, "Pss:"), None);
        }
        let value = process_snapshot();
        assert_eq!(value["pid"], std::process::id());
        let expected: &[&str] = if cfg!(target_os = "macos") {
            &["footprint", "rss"]
        } else if cfg!(target_os = "linux") {
            &["rss", "pss"]
        } else if cfg!(windows) {
            &["working_set", "private_bytes"]
        } else {
            &[]
        };
        for key in expected {
            assert!(
                value["metrics"][key]["bytes"].as_u64().is_some()
                    || value["metrics"][key]["unavailable_reason"]
                        .as_str()
                        .is_some_and(|s| s != "Not provided on this platform")
            );
        }

        for metric in value["metrics"].as_object().unwrap().values() {
            assert!(metric["bytes"].as_u64().is_some() || metric["unavailable_reason"].is_string());
        }
    }
}
