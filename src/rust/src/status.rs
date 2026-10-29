//! Writes /var/local/cache.dirs2/status.json for the settings page.

use std::fs;
use std::path::Path;

#[derive(Clone, Debug, Default)]
pub struct TargetStatus {
    pub name: String,
    pub mount: String,
    pub kind: String,
    pub fs: String,
    pub devices: Vec<String>,
    /// "awake", "asleep" or "unknown"
    pub state: String,
    pub entries: u64,
    pub errors: u64,
    pub last_refresh: u64,
    pub last_ms: u64,
    pub last_reads: u64,
    pub wakes: u64,
    pub last_wake: u64,
    pub refills: u64,
    pub backoff_until: u64,
    pub backoff_secs: u64,
    /// why the last cycle skipped this target ("" if it was refreshed)
    pub skipped: String,
    pub noatime: bool,
    /// false where wakes can't be attributed exactly (ZFS): none are reported
    pub wake_detect: bool,
    pub seen: bool,
    /// read counter at the end of our last look at this target
    pub last_counter: Option<u64>,
    /// top-level folders seen on this target at its last refresh
    pub top: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Status {
    pub version: String,
    pub pid: u32,
    pub started: u64,
    pub cycle: u64,
    pub last_cycle: u64,
    pub interval: u64,
    pub mem_total: u64,
    pub cache_bytes: u64,
    pub budget_pct: u64,
    pub message: String,
    pub targets: Vec<TargetStatus>,
    /// union of top-level folders on all cached targets
    pub folders: Vec<String>,
}

pub fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

impl Status {
    pub fn to_json(&self) -> String {
        let targets: Vec<String> = self
            .targets
            .iter()
            .filter(|t| t.seen)
            .map(|t| {
                let devs: Vec<String> = t.devices.iter().map(|d| esc(d)).collect();
                format!(
                    "{{\"name\":{},\"mount\":{},\"kind\":{},\"fs\":{},\"devices\":[{}],\"state\":{},\"entries\":{},\"errors\":{},\"last_refresh\":{},\"last_ms\":{},\"last_reads\":{},\"wakes\":{},\"last_wake\":{},\"refills\":{},\"backoff_until\":{},\"skipped\":{},\"noatime\":{},\"wake_detect\":{}}}",
                    esc(&t.name), esc(&t.mount), esc(&t.kind), esc(&t.fs), devs.join(","), esc(&t.state),
                    t.entries, t.errors, t.last_refresh, t.last_ms, t.last_reads, t.wakes, t.last_wake,
                    t.refills, t.backoff_until, esc(&t.skipped), t.noatime, t.wake_detect
                )
            })
            .collect();
        format!(
            "{{\"version\":{},\"pid\":{},\"started\":{},\"cycle\":{},\"last_cycle\":{},\"interval\":{},\"mem_total\":{},\"cache_bytes\":{},\"budget_pct\":{},\"message\":{},\"folders\":[{}],\"targets\":[{}]}}\n",
            esc(&self.version), self.pid, self.started, self.cycle, self.last_cycle, self.interval,
            self.mem_total, self.cache_bytes, self.budget_pct, esc(&self.message),
            self.folders.iter().map(|f| esc(f)).collect::<Vec<_>>().join(","), targets.join(",")
        )
    }

    /// Atomic write: temp file + rename, so the page never reads half a file.
    pub fn write(&self, dir: &Path) {
        if fs::create_dir_all(dir).is_err() {
            return;
        }
        let tmp = dir.join("status.json.tmp");
        if fs::write(&tmp, self.to_json()).is_ok() {
            let _ = fs::rename(&tmp, dir.join("status.json"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn escapes() {
        assert_eq!(esc("a\"b\\c\n"), "\"a\\\"b\\\\c\\n\"");
    }
}
