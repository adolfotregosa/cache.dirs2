//! Finds what to cache (array disks, pools, extra mount points), which physical
//! devices sit behind each one, whether those devices are spun down, and how many
//! reads they have completed (from /sys/block/<dev>/stat).

use crate::config::{Config, TargetMode};
use crate::warn;
use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

/// Extra mount points we already warned about, so a missing one is logged once
/// instead of every cycle. Cleared when it comes back.
static MISSING_WARNED: Mutex<Option<HashSet<PathBuf>>> = Mutex::new(None);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Array,
    Pool,
    Extra,
}

impl Kind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Kind::Array => "array",
            Kind::Pool => "pool",
            Kind::Extra => "extra",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Target {
    pub name: String,
    pub mount: PathBuf,
    pub kind: Kind,
    pub devices: Vec<String>,
    pub fs: String,
    pub noatime: bool,
    /// mounts below this target that belong to something else and must not be walked
    pub foreign: HashSet<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct Mount {
    pub source: String,
    pub target: PathBuf,
    pub fstype: String,
    pub opts: String,
}

/// Where emhttp keeps disks.ini. Overridable for testing.
pub fn emhttp_dir() -> PathBuf {
    env::var_os("CD2_EMHTTP")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/var/local/emhttp"))
}

/// Root of the mount points (normally /mnt). Overridable for testing.
pub fn mnt_root() -> PathBuf {
    env::var_os("CD2_MNT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/mnt"))
}

pub type IniSection = (String, HashMap<String, String>);

pub fn parse_ini(text: &str) -> Vec<IniSection> {
    let mut out: Vec<IniSection> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') && line.ends_with(']') {
            let name = line[1..line.len() - 1].trim().trim_matches('"').to_string();
            out.push((name, HashMap::new()));
        } else if let Some((k, v)) = line.split_once('=') {
            if let Some(last) = out.last_mut() {
                let v = v.trim();
                let v = v.strip_prefix('"').unwrap_or(v);
                let v = v.strip_suffix('"').unwrap_or(v);
                last.1.insert(k.trim().to_string(), v.to_string());
            }
        }
    }
    out
}

pub fn read_disks_ini() -> Vec<IniSection> {
    fs::read_to_string(emhttp_dir().join("disks.ini"))
        .map(|t| parse_ini(&t))
        .unwrap_or_default()
}

fn unescape_mount(s: &str) -> String {
    // /proc/mounts escapes space, tab, newline and backslash as octal.
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 4 <= b.len() {
            let oct = &b[i + 1..i + 4];
            if oct.iter().all(|c| (b'0'..=b'7').contains(c)) {
                if let Ok(v) = u8::from_str_radix(std::str::from_utf8(oct).unwrap_or("x"), 8) {
                    out.push(v);
                    i += 4;
                    continue;
                }
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn mounts() -> Vec<Mount> {
    let text = fs::read_to_string("/proc/mounts").unwrap_or_default();
    text.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            if f.len() < 4 {
                return None;
            }
            Some(Mount {
                source: unescape_mount(f[0]),
                target: PathBuf::from(unescape_mount(f[1])),
                fstype: f[2].to_string(),
                opts: f[3].to_string(),
            })
        })
        .collect()
}

/// The mount that contains `path` (longest matching mount point).
pub fn mount_for<'a>(path: &Path, mounts: &'a [Mount]) -> Option<&'a Mount> {
    mounts
        .iter()
        .filter(|m| path.starts_with(&m.target))
        .max_by_key(|m| m.target.as_os_str().len())
}

/// Mounts below `root` that are NOT part of the same disk. ZFS datasets of the same
/// pool (Unraid creates one per share on ZFS disks) and re-mounts of the same
/// source (btrfs subvolumes) count as the same disk; everything else is foreign.
pub fn foreign_mounts(root: &Path, mounts: &[Mount]) -> HashSet<PathBuf> {
    let base = match mounts.iter().filter(|m| m.target == root).last() {
        Some(m) => m,
        None => return HashSet::new(),
    };
    let pool = |src: &str| src.split('/').next().unwrap_or("").to_string();
    mounts
        .iter()
        .filter(|m| m.target != root && m.target.starts_with(root))
        .filter(|m| {
            let same = m.source == base.source
                || (base.fstype == "zfs" && m.fstype == "zfs" && pool(&m.source) == pool(&base.source));
            !same
        })
        .map(|m| m.target.clone())
        .collect()
}

/// Turns a partition or device-mapper name into the physical disk(s) behind it.
pub fn physical_devices(name: &str) -> Vec<String> {
    let sys = Path::new("/sys/class/block").join(name);
    // device-mapper (LUKS etc.): follow slaves
    let slaves = sys.join("slaves");
    if let Ok(rd) = fs::read_dir(&slaves) {
        let mut out = Vec::new();
        for e in rd.flatten() {
            out.extend(physical_devices(&e.file_name().to_string_lossy()));
        }
        if !out.is_empty() {
            return out;
        }
    }
    if sys.join("partition").exists() {
        if let Ok(real) = fs::canonicalize(&sys) {
            if let Some(parent) = real.parent().and_then(|p| p.file_name()) {
                return vec![parent.to_string_lossy().into_owned()];
            }
        }
    }
    vec![name.to_string()]
}

pub fn devices_from_source(source: &str) -> Vec<String> {
    if !source.starts_with("/dev/") {
        return Vec::new(); // zfs dataset, tmpfs, network share...
    }
    let real = fs::canonicalize(source).unwrap_or_else(|_| PathBuf::from(source));
    match real.file_name() {
        Some(n) => physical_devices(&n.to_string_lossy()),
        None => Vec::new(),
    }
}

pub fn rotational(dev: &str) -> Option<bool> {
    let p = Path::new("/sys/block").join(dev).join("queue/rotational");
    fs::read_to_string(p).ok().map(|s| s.trim() == "1")
}

/// Sum of completed reads AND writes across devices: any I/O means the disk is
/// (or recently was) spinning.
pub fn io_count(devs: &[String]) -> u64 {
    devs.iter()
        .filter_map(|d| fs::read_to_string(Path::new("/sys/block").join(d).join("stat")).ok())
        .map(|s| {
            let f: Vec<u64> = s.split_whitespace().filter_map(|x| x.parse().ok()).collect();
            f.first().copied().unwrap_or(0) + f.get(4).copied().unwrap_or(0)
        })
        .sum()
}

/// Bytes the calling thread has caused to be read from storage
/// (`read_bytes` in /proc/thread-self/io). None if the kernel doesn't provide it.
pub fn thread_read_bytes() -> Option<u64> {
    fs::read_to_string("/proc/thread-self/io")
        .ok()?
        .lines()
        .find_map(|l| l.strip_prefix("read_bytes:"))
        .and_then(|v| v.trim().parse().ok())
}

/// Requests the devices are working on right now (field 9 of /sys/block/<dev>/stat).
/// A "sleeping" disk with requests in flight is being woken by someone else.
pub fn in_flight(devs: &[String]) -> u64 {
    devs.iter()
        .filter_map(|d| fs::read_to_string(Path::new("/sys/block").join(d).join("stat")).ok())
        .filter_map(|s| s.split_whitespace().nth(8).and_then(|f| f.parse::<u64>().ok()))
        .sum()
}

/// Asks each drive directly whether it is spun down (ATA CHECK POWER MODE, which
/// does not wake the drive). Some(true) if any is in standby, Some(false) if every
/// drive that answered is active and at least one answered, None if none answered.
pub fn confirm_asleep(devs: &[String]) -> Option<bool> {
    let mut known = false;
    for d in devs {
        match hdparm_asleep(d) {
            Some(true) => return Some(true),
            Some(false) => known = true,
            None => {}
        }
    }
    if known {
        Some(false)
    } else {
        None
    }
}

/// Sum of completed reads across devices.
pub fn reads(devs: &[String]) -> u64 {
    devs.iter()
        .filter_map(|d| fs::read_to_string(Path::new("/sys/block").join(d).join("stat")).ok())
        .filter_map(|s| s.split_whitespace().next().and_then(|f| f.parse::<u64>().ok()))
        .sum()
}

fn is_array_name(n: &str) -> bool {
    n.len() > 4 && n.starts_with("disk") && n[4..].bytes().all(|c| c.is_ascii_digit())
}

fn is_mounted(p: &Path, mounts: &[Mount]) -> bool {
    if env::var_os("CD2_ASSUME_MOUNTED").is_some() {
        return p.is_dir();
    }
    mounts.iter().any(|m| m.target == p)
}

pub fn discover(cfg: &Config) -> Vec<Target> {
    let mounts = mounts();
    let ini = read_disks_ini();
    let root = mnt_root();
    let mut out: Vec<Target> = Vec::new();

    for (sec, kv) in &ini {
        let name = kv.get("name").cloned().unwrap_or_else(|| sec.clone());
        let typ = kv.get("type").map(|s| s.as_str()).unwrap_or("");
        if name.is_empty() || typ == "Parity" || typ == "Flash" || name.starts_with("parity") || name == "flash" {
            continue;
        }
        let mount = root.join(&name);
        if !is_mounted(&mount, &mounts) {
            continue;
        }
        let kind = if is_array_name(&name) { Kind::Array } else { Kind::Pool };
        let mut devices: Vec<String> = Vec::new();
        if let Some(d) = kv.get("device").filter(|d| !d.is_empty()) {
            devices.push(d.clone());
        }
        if kind == Kind::Pool {
            // other members of a multi-device pool are listed as <pool>2, <pool>3, ...
            for (sec2, kv2) in &ini {
                let n2 = kv2.get("name").cloned().unwrap_or_else(|| sec2.clone());
                if n2.len() > name.len()
                    && n2.starts_with(&name)
                    && n2[name.len()..].bytes().all(|c| c.is_ascii_digit())
                    && !is_mounted(&root.join(&n2), &mounts)
                {
                    if let Some(d) = kv2.get("device").filter(|d| !d.is_empty()) {
                        devices.push(d.clone());
                    }
                }
            }
        }
        let m = mount_for(&mount, &mounts);
        // The kernel's mount table has the real filesystem. disks.ini prefixes
        // encrypted disks ("luks:zfs"), which would hide that a disk is ZFS.
        let fs = m
            .map(|m| m.fstype.clone())
            .filter(|f| !f.is_empty())
            .or_else(|| kv.get("fsType").map(|f| f.trim_start_matches("luks:").to_string()))
            .unwrap_or_default();
        let noatime = m.map(|m| m.opts.split(',').any(|o| o == "noatime")).unwrap_or(true);
        let foreign = foreign_mounts(&mount, &mounts);
        out.push(Target { name, mount, kind, devices, fs, noatime, foreign });
    }

    for p in &cfg.extra {
        let mut warned = MISSING_WARNED.lock().unwrap_or_else(|e| e.into_inner());
        let warned = warned.get_or_insert_with(HashSet::new);
        if !p.is_dir() {
            if warned.insert(p.clone()) {
                warn!("extra mount point {} does not exist or is not a directory, skipping it until it appears", p.display());
            }
            continue;
        }
        warned.remove(p);
        let m = mount_for(p, &mounts);
        let devices = m.map(|m| devices_from_source(&m.source)).unwrap_or_default();
        out.push(Target {
            name: p.display().to_string(),
            mount: p.clone(),
            kind: Kind::Extra,
            devices,
            fs: m.map(|m| m.fstype.clone()).unwrap_or_default(),
            noatime: m.map(|m| m.opts.split(',').any(|o| o == "noatime")).unwrap_or(true),
            foreign: foreign_mounts(p, &mounts),
        });
    }

    let mut seen = HashSet::new();
    out.retain(|t| seen.insert(t.mount.clone()));
    out.retain(|t| match cfg.targets {
        TargetMode::All => true,
        TargetMode::Array => t.kind != Kind::Pool,
        // Hard drives only: skip array disks and pools made only of SSD/NVMe, which
        // never spin down. Extra mount points were added by hand, so they stay.
        TargetMode::Rotational => {
            t.kind == Kind::Extra
                || t.devices.is_empty()
                || t.devices.iter().any(|d| rotational(d) != Some(false))
        }
    });
    out.sort_by(|a, b| a.kind.cmp(&b.kind).then_with(|| natural_key(&a.name).cmp(&natural_key(&b.name))));
    out
}

/// "disk2" sorts before "disk10".
fn natural_key(s: &str) -> (String, u64) {
    let digits: String = s.chars().rev().take_while(|c| c.is_ascii_digit()).collect::<Vec<_>>().into_iter().rev().collect();
    let prefix = s[..s.len() - digits.len()].to_string();
    (prefix, digits.parse().unwrap_or(0))
}

/// device -> spun down, as reported by emhttp.
pub fn spin_map() -> HashMap<String, bool> {
    let mut m = HashMap::new();
    for (_, kv) in read_disks_ini() {
        if let (Some(d), Some(s)) = (kv.get("device"), kv.get("spundown")) {
            if !d.is_empty() {
                m.insert(d.clone(), s == "1");
            }
        }
    }
    m
}

/// Asks the drive for its power state without waking it (ATA CHECK POWER MODE).
fn hdparm_asleep(dev: &str) -> Option<bool> {
    if !dev.starts_with("sd") {
        return None;
    }
    let out = Command::new("hdparm").arg("-C").arg(format!("/dev/{}", dev)).output().ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    if s.contains("standby") || s.contains("sleeping") {
        Some(true)
    } else if s.contains("active") || s.contains("idle") {
        Some(false)
    } else {
        None
    }
}

/// Some(true) if any device behind the target is spun down, Some(false) if all
/// known devices are spinning, None if the state can't be determined.
pub fn asleep(devs: &[String], map: &HashMap<String, bool>) -> Option<bool> {
    let mut known = false;
    for d in devs {
        let state = map.get(d).copied().or_else(|| hdparm_asleep(d));
        match state {
            Some(true) => return Some(true),
            Some(false) => known = true,
            None => {}
        }
    }
    if known {
        Some(false)
    } else {
        None
    }
}

pub fn mover_running() -> bool {
    match fs::read_to_string("/var/run/mover.pid") {
        Ok(pid) => Path::new("/proc").join(pid.trim()).exists(),
        Err(_) => false,
    }
}

pub fn mem_total() -> u64 {
    fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|t| {
            t.lines()
                .find(|l| l.starts_with("MemTotal:"))
                .and_then(|l| l.split_whitespace().nth(1).and_then(|v| v.parse::<u64>().ok()))
        })
        .map(|kb| kb * 1024)
        .unwrap_or(0)
}

/// Bytes currently held by the kernel's dentry and inode caches (all filesystems).
pub fn metadata_cache_bytes() -> u64 {
    const CACHES: &[&str] = &[
        "dentry",
        "inode_cache",
        "xfs_inode",
        "btrfs_inode",
        "ext4_inode_cache",
        "zfs_znode_cache",
        "dnode_t",
        "fuse_inode",
    ];
    let text = fs::read_to_string("/proc/slabinfo").unwrap_or_default();
    text.lines()
        .skip(2)
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            if f.len() < 4 || !CACHES.contains(&f[0]) {
                return None;
            }
            let num: u64 = f[2].parse().ok()?;
            let size: u64 = f[3].parse().ok()?;
            Some(num * size)
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ini_and_names() {
        let ini = parse_ini("[\"disk1\"]\nname=\"disk1\"\ndevice=\"sdc\"\nspundown=\"1\"\n[cache]\nname=\"cache\"\n");
        assert_eq!(ini.len(), 2);
        assert_eq!(ini[0].1["device"], "sdc");
        assert!(is_array_name("disk12"));
        assert!(!is_array_name("disks"));
        assert!(natural_key("disk2") < natural_key("disk10"));
    }

    #[test]
    fn foreign() {
        let mk = |src: &str, tgt: &str, fs: &str| Mount {
            source: src.into(),
            target: PathBuf::from(tgt),
            fstype: fs.into(),
            opts: "rw".into(),
        };
        let mounts = vec![
            mk("disk1", "/mnt/disk1", "zfs"),
            mk("disk1/Media", "/mnt/disk1/Media", "zfs"),
            mk("//nas/share", "/mnt/disk1/Media/remote", "cifs"),
            mk("/dev/md2p1", "/mnt/disk2", "xfs"),
        ];
        let f = foreign_mounts(Path::new("/mnt/disk1"), &mounts);
        assert!(!f.contains(Path::new("/mnt/disk1/Media")));
        assert!(f.contains(Path::new("/mnt/disk1/Media/remote")));
        assert!(!f.contains(Path::new("/mnt/disk2")));
    }

    #[test]
    fn stat_fields() {
        // reads=100 ... writes=40 ... in_flight=3
        let line = "100 0 800 10 40 0 320 5 3 15 15 0 0 0 0 0 0";
        let f: Vec<u64> = line.split_whitespace().filter_map(|x| x.parse().ok()).collect();
        assert_eq!(f[0] + f[4], 140);
        assert_eq!(line.split_whitespace().nth(8).unwrap(), "3");
    }

    #[test]
    fn unescape() {
        assert_eq!(unescape_mount("/mnt/My\\040Disk"), "/mnt/My Disk");
        assert_eq!(unescape_mount("plain"), "plain");
    }
}
