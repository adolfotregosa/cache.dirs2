//! Protects the cached metadata from reclaim with a cgroup v2 memory.min.
//!
//! The daemon runs inside its own group. Everything it loads (dentries, inodes,
//! and the directory and inode-table blocks in the file cache) is charged to that
//! group, and memory.min tells the kernel to reclaim everything else first.
//! ZFS keeps its metadata in the ARC, which is not charged to any group, so ZFS
//! gets no benefit from this.
//!
//! The service script keeps a placeholder process in the group, because Unraid
//! removes empty cgroups within seconds; without it a daemon restart would drop
//! the protection on everything already cached.

use std::env;
use std::fs;
use std::path::PathBuf;

pub fn root() -> PathBuf {
    env::var_os("CD2_CGROUP_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/sys/fs/cgroup"))
}

pub fn group() -> PathBuf {
    root().join("cachedirs2")
}

fn has_word(path: PathBuf, word: &str) -> bool {
    fs::read_to_string(path)
        .map(|s| s.split_whitespace().any(|w| w == word))
        .unwrap_or(false)
}

/// Moves this process into the group, creating it if needed.
pub fn join() -> Result<(), String> {
    let root = root();
    if !root.join("cgroup.controllers").exists() {
        return Err("cgroup v2 is not available".into());
    }
    if !has_word(root.join("cgroup.controllers"), "memory") {
        return Err("the cgroup memory controller is not available".into());
    }
    if !has_word(root.join("cgroup.subtree_control"), "memory") {
        fs::write(root.join("cgroup.subtree_control"), "+memory")
            .map_err(|e| format!("can't enable the cgroup memory controller: {}", e))?;
    }
    let g = group();
    fs::create_dir_all(&g).map_err(|e| format!("can't create {}: {}", g.display(), e))?;
    fs::write(g.join("cgroup.procs"), std::process::id().to_string())
        .map_err(|e| format!("can't join {}: {}", g.display(), e))?;
    Ok(())
}

fn read_u64(name: &str) -> Option<u64> {
    let s = fs::read_to_string(group().join(name)).ok()?;
    let s = s.trim();
    if s == "max" {
        Some(u64::MAX)
    } else {
        s.parse().ok()
    }
}

/// Memory currently charged to the group.
pub fn current() -> Option<u64> {
    read_u64("memory.current")
}

/// Current protection.
pub fn min() -> Option<u64> {
    read_u64("memory.min")
}

pub fn set_min(bytes: u64) -> Result<(), String> {
    fs::write(group().join("memory.min"), bytes.to_string())
        .map_err(|e| format!("can't set memory.min: {}", e))
}

/// Protection to ask for: the group's footprint with headroom for growth,
/// at least `floor`, never more than `cap`.
pub fn target(current: u64, floor: u64, cap: u64) -> u64 {
    let want = (current + current / 2).max(current + floor);
    want.min(cap.max(floor))
}

/// Only change memory.min when it matters: raise as soon as the target is higher,
/// lower only once the target has fallen well below it.
pub fn needs_update(have: u64, want: u64) -> bool {
    want > have || want < have / 10 * 7
}

#[cfg(test)]
mod tests {
    use super::*;
    const MIB: u64 = 1 << 20;

    #[test]
    fn sizing() {
        // small footprint: floor headroom
        assert_eq!(target(100 * MIB, 256 * MIB, 32 * 1024 * MIB), 356 * MIB);
        // large footprint: 50% headroom
        assert_eq!(target(1000 * MIB, 256 * MIB, 32 * 1024 * MIB), 1500 * MIB);
        // capped
        assert_eq!(target(1000 * MIB, 256 * MIB, 1200 * MIB), 1200 * MIB);
        assert!(needs_update(300 * MIB, 400 * MIB));
        assert!(!needs_update(400 * MIB, 350 * MIB));
        assert!(needs_update(400 * MIB, 200 * MIB));
    }
}
