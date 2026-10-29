//! Reads the plugin's key="value" config files. Later files override earlier ones,
//! so the plugin passes default.cfg first and the user's cfg on the flash drive second.

use std::fs;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TargetMode {
    /// Array disks and pools that contain at least one spinning (rotational) device.
    Rotational,
    /// Array disks only (plus extra mount points).
    Array,
    /// Everything mounted: array and all pools.
    All,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub targets: TargetMode,
    pub extra: Vec<PathBuf>,
    pub interval: u64,
    pub refresh_sleeping: bool,
    pub prime: bool,
    pub max_depth: usize,
    pub budget_pct: u64,
    pub verbose: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            include: Vec::new(),
            exclude: Vec::new(),
            targets: TargetMode::Rotational,
            extra: Vec::new(),
            interval: 30,
            refresh_sleeping: true,
            prime: true,
            max_depth: 0,
            budget_pct: 25,
            verbose: false,
        }
    }
}

pub fn split_list(v: &str) -> Vec<String> {
    v.split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

pub fn parse_kv(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') || line.starts_with('[') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let v = v.trim();
            let v = v.strip_prefix('"').unwrap_or(v);
            let v = v.strip_suffix('"').unwrap_or(v);
            out.push((k.trim().to_string(), v.to_string()));
        }
    }
    out
}

impl Config {
    pub fn load(paths: &[PathBuf]) -> Config {
        let mut c = Config::default();
        for p in paths {
            if let Ok(text) = fs::read_to_string(p) {
                for (k, v) in parse_kv(&text) {
                    c.apply(&k, &v);
                }
            }
        }
        c
    }

    fn apply(&mut self, k: &str, v: &str) {
        match k {
            "include" => self.include = split_list(v),
            "exclude" => self.exclude = split_list(v),
            "targets" => {
                self.targets = match v {
                    "array" => TargetMode::Array,
                    "all" => TargetMode::All,
                    _ => TargetMode::Rotational,
                }
            }
            "extra" => self.extra = split_list(v).into_iter().map(PathBuf::from).collect(),
            "interval" => {
                if let Ok(n) = v.parse::<u64>() {
                    self.interval = n.clamp(5, 3600);
                }
            }
            "sleeping" => self.refresh_sleeping = v != "skip",
            "prime" => self.prime = v == "1",
            "maxdepth" => {
                if let Ok(n) = v.parse::<usize>() {
                    self.max_depth = n;
                }
            }
            "budget" => {
                if let Ok(n) = v.parse::<u64>() {
                    self.budget_pct = n.clamp(1, 90);
                }
            }
            "verbose" => self.verbose = v == "1",
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cfg() {
        let kv = parse_kv("service=\"1\"\ninclude=\"Media, My Photos\"\n# comment\ninterval=\"2\"\n");
        let mut c = Config::default();
        for (k, v) in kv {
            c.apply(&k, &v);
        }
        assert_eq!(c.include, vec!["Media", "My Photos"]);
        assert_eq!(c.interval, 5); // clamped
    }
}
