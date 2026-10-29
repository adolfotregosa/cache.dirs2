//! cachedirs2 — Cache Directories 2.0 for Unraid.
//!
//! Keeps directory listings AND file metadata (inodes) in RAM so browsing shares
//! doesn't spin disks up. Rules:
//!   * every entry is stat'd, not just directories;
//!   * spinning disks are refreshed every cycle (cheap, keeps the cache hot);
//!   * sleeping disks are refreshed only if allowed. Each refresh measures what its
//!     own thread read from disk (per-thread I/O accounting), so other programs
//!     using the disk are never blamed on it. If a refresh of a sleeping disk had to
//!     read, the cache had been evicted and the refresh woke the disk: that is
//!     logged and the disk is put on a back-off so it can't happen every cycle.
//!     On ZFS, which reads from its own kernel threads, this can't be measured, so
//!     no wakes are reported there.

mod config;
mod disks;
mod log;
mod status;
mod walk;

use config::Config;
use disks::Target;
use status::{Status, TargetStatus};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Plugin version, injected by build.sh (CD2_VERSION); falls back to the crate version.
const VERSION: &str = match option_env!("CD2_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};
const STATUS_DIR: &str = "/var/local/cache.dirs2";
const BACKOFF_MIN: u64 = 3600;
const BACKOFF_MAX: u64 = 24 * 3600;

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn human_bytes(b: u64) -> String {
    let units = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < units.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    format!("{:.1} {}", v, units[i])
}

fn usage() {
    println!(
        "cachedirs2 {VERSION} - keeps Unraid folder listings and file metadata in RAM

Usage:
  cachedirs2 [--config FILE]... [--foreground] [--once]
      Run the caching daemon. Config files are read in order; later ones win.
  cachedirs2 --targets [--config FILE]...
      Show what would be cached, which devices back it, and their spin state.
  cachedirs2 --scan PATH [--depth N]
      Walk PATH once and report entries, time, and disk reads. Run it twice:
      the second run should show 0 reads if the metadata stayed cached.
  cachedirs2 --version"
    );
}

struct Args {
    configs: Vec<PathBuf>,
    foreground: bool,
    once: bool,
    scan: Option<PathBuf>,
    depth: usize,
    targets: bool,
}

fn parse_args() -> Args {
    let mut a = Args { configs: Vec::new(), foreground: false, once: false, scan: None, depth: 0, targets: false };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--config" | "-c" => {
                if let Some(v) = it.next() {
                    a.configs.push(PathBuf::from(v));
                }
            }
            "--foreground" | "-F" => a.foreground = true,
            "--once" => a.once = true,
            "--scan" => a.scan = it.next().map(PathBuf::from),
            "--depth" => a.depth = it.next().and_then(|v| v.parse().ok()).unwrap_or(0),
            "--targets" => a.targets = true,
            "--version" | "-V" => {
                println!("cachedirs2 {VERSION}");
                std::process::exit(0);
            }
            _ => {
                usage();
                std::process::exit(if arg == "--help" || arg == "-h" { 0 } else { 2 });
            }
        }
    }
    a
}

fn run_scan(path: &Path, depth: usize) {
    let mounts = disks::mounts();
    let devs = disks::mount_for(path, &mounts)
        .map(|m| {
            // array disks are mounted from /dev/mdXp1; ask emhttp for the real device
            if m.source.starts_with("/dev/md") {
                let name = m.target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                disks::read_disks_ini()
                    .into_iter()
                    .find(|(s, kv)| kv.get("name").unwrap_or(s) == &name)
                    .and_then(|(_, kv)| kv.get("device").cloned())
                    .filter(|d| !d.is_empty())
                    .map(|d| vec![d])
                    .unwrap_or_default()
            } else {
                disks::devices_from_source(&m.source)
            }
        })
        .unwrap_or_default();
    let state = disks::asleep(&devs, &disks::spin_map());
    let before = disks::reads(&devs);
    let foreign = disks::foreign_mounts(
        disks::mount_for(path, &mounts).map(|m| m.target.as_path()).unwrap_or(path),
        &mounts,
    );
    let r = walk::walk(path, &[], &[], depth, &foreign);
    let after = disks::reads(&devs);
    println!("path:      {}", path.display());
    println!(
        "devices:   {}",
        if devs.is_empty() { "unknown (reads can't be measured)".to_string() } else { devs.join(", ") }
    );
    println!(
        "state:     {}",
        match state {
            Some(true) => "asleep before scan",
            Some(false) => "spinning",
            None => "unknown",
        }
    );
    println!("entries:   {} ({} folders, {} files, {} errors)", r.entries(), r.dirs, r.files, r.errors);
    println!("time:      {:.2}s", r.elapsed.as_secs_f64());
    if !devs.is_empty() {
        println!("reads:     {}  {}", after - before, if after == before { "(all from RAM)" } else { "(some metadata came from disk)" });
    }
    println!("metadata cache now: {}", human_bytes(disks::metadata_cache_bytes()));
}

fn run_targets(cfg: &Config) {
    let targets = disks::discover(cfg);
    if targets.is_empty() {
        println!("No targets found. Is the array started?");
        return;
    }
    let spins = disks::spin_map();
    for t in &targets {
        let state = match disks::asleep(&t.devices, &spins) {
            Some(true) => "asleep",
            Some(false) => "spinning",
            None => "unknown",
        };
        println!(
            "{:<12} {:<6} {:<28} fs={:<6} devices=[{}] {}{}",
            t.name,
            t.kind.as_str(),
            t.mount.display(),
            t.fs,
            t.devices.join(","),
            state,
            if t.noatime { "" } else { "  (not mounted noatime)" }
        );
    }
}

struct Job {
    target: Target,
    state: Option<bool>,
}

struct Outcome {
    target: Target,
    state: Option<bool>,
    result: walk::WalkResult,
    reads: u64,
    counter: u64,
    /// bytes this refresh's own thread read from storage (None: not measurable)
    own_bytes: Option<u64>,
}

fn main() {
    let args = parse_args();
    log::FOREGROUND.store(args.foreground || args.scan.is_some() || args.targets, Ordering::Relaxed);
    let cfg = Config::load(&args.configs);
    log::VERBOSE.store(cfg.verbose, Ordering::Relaxed);

    if let Some(p) = &args.scan {
        run_scan(p, args.depth);
        return;
    }
    if args.targets {
        run_targets(&cfg);
        return;
    }
    daemon(cfg, args.once);
}

fn daemon(cfg: Config, once: bool) {
    let status_dir = Path::new(STATUS_DIR);
    let mut st = Status {
        version: VERSION.to_string(),
        pid: std::process::id(),
        started: now(),
        interval: cfg.interval,
        budget_pct: cfg.budget_pct,
        mem_total: disks::mem_total(),
        ..Default::default()
    };
    let mut per: HashMap<PathBuf, TargetStatus> = HashMap::new();
    let mut primed: HashMap<PathBuf, bool> = HashMap::new();
    let mut atime_warned: HashMap<PathBuf, bool> = HashMap::new();
    let mut last_budget_warn = 0u64;

    info!(
        "v{} started: interval {}s, sleeping disks: {}, prime: {}, include: [{}], exclude: [{}], extra: [{}], max depth: {}",
        VERSION,
        cfg.interval,
        if cfg.refresh_sleeping { "refresh" } else { "skip" },
        if cfg.prime { "yes" } else { "no" },
        cfg.include.join(", "),
        cfg.exclude.join(", "),
        cfg.extra.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", "),
        if cfg.max_depth == 0 { "unlimited".to_string() } else { cfg.max_depth.to_string() }
    );

    loop {
        st.cycle += 1;
        let t_now = now();

        if disks::mover_running() {
            st.message = "Paused while the mover runs.".into();
            debug!("mover running, pausing");
            st.last_cycle = t_now;
            st.targets = per.values().cloned().collect();
            sort_targets(&mut st.targets);
            st.write(status_dir);
            if once {
                return;
            }
            thread::sleep(Duration::from_secs(10));
            continue;
        }

        let targets = disks::discover(&cfg);
        if targets.is_empty() {
            let mut all = cfg.clone();
            all.targets = config::TargetMode::All;
            st.message = if disks::discover(&all).is_empty() {
                "No mounted disks found. Caching starts when the array is started.".into()
            } else {
                "Nothing to cache: no mounted disk matches \"Disks to cache\". \
                 Solid-state disks are skipped by the default setting."
                    .into()
            };
        } else {
            st.message.clear();
        }
        for ts in per.values_mut() {
            ts.seen = false;
        }

        let spins = disks::spin_map();
        let mut jobs: Vec<Job> = Vec::new();
        for t in targets {
            let mut state = disks::asleep(&t.devices, &spins);
            let ts = per.entry(t.mount.clone()).or_default();
            // emhttp updates spin state on its own schedule. If the disk has done
            // reads since we last looked, it is awake whatever disks.ini says; that
            // stops someone else's spin-up from being blamed on us.
            let counter = disks::io_count(&t.devices);
            if state == Some(true) && ts.last_counter.map_or(false, |c| c != counter) {
                state = Some(false);
            }
            ts.last_counter = Some(counter);
            // Unraid's spin state can lag a spin-up by several seconds, and a spin-up
            // (from the web UI, SMART, a spin-up group...) does no I/O. Ask the drive.
            if state == Some(true) && disks::in_flight(&t.devices) > 0 {
                debug!("{}: Unraid reports it asleep but another program is spinning it up", t.name);
                state = Some(false);
            }
            if state == Some(true) && disks::confirm_asleep(&t.devices) == Some(false) {
                debug!("{}: Unraid reports it asleep but the drive is spinning", t.name);
                state = Some(false);
            }
            ts.name = t.name.clone();
            ts.mount = t.mount.display().to_string();
            ts.kind = t.kind.as_str().to_string();
            ts.fs = t.fs.clone();
            ts.devices = t.devices.clone();
            ts.noatime = t.noatime;
            ts.wake_detect = wake_detectable(&t.fs);
            ts.seen = true;
            ts.state = match state {
                Some(true) => "asleep",
                Some(false) => "awake",
                None => "unknown",
            }
            .into();

            if !t.noatime && !atime_warned.contains_key(&t.mount) {
                warn!(
                    "{} is not mounted with noatime; reading folders may write access times and wake the disk",
                    t.mount.display()
                );
                atime_warned.insert(t.mount.clone(), true);
            }

            let is_primed = primed.get(&t.mount).copied().unwrap_or(false);
            let asleep = state == Some(true);
            let go = if !asleep {
                true
            } else if !is_primed {
                if cfg.prime {
                    true
                } else {
                    ts.skipped = "asleep, will be cached once it spins up".into();
                    false
                }
            } else if !cfg.refresh_sleeping {
                ts.skipped = "asleep (refresh of sleeping disks is off)".into();
                false
            } else if ts.backoff_until > t_now {
                ts.skipped = "asleep, backing off after a recent wake".into();
                false
            } else {
                true
            };
            if go {
                ts.skipped.clear();
                jobs.push(Job { target: t, state });
            }
        }

        // A target that disappeared (unmounted) must be primed again when it returns,
        // otherwise its first refresh would be reported as a wake.
        for (mount, ts) in per.iter_mut() {
            if !ts.seen {
                primed.remove(mount);
                ts.last_counter = None;
            }
        }

        // Refresh every target in parallel; each one is a separate disk.
        let outcomes: Vec<Outcome> = thread::scope(|s| {
            let handles: Vec<_> = jobs
                .into_iter()
                .map(|job| {
                    let cfg = &cfg;
                    s.spawn(move || {
                        let own_before = disks::thread_read_bytes();
                        let before = disks::reads(&job.target.devices);
                        let result = walk::walk(
                            &job.target.mount,
                            &cfg.include,
                            &cfg.exclude,
                            cfg.max_depth,
                            &job.target.foreign,
                        );
                        let after = disks::reads(&job.target.devices);
                        let counter = disks::io_count(&job.target.devices);
                        let own_bytes = match (own_before, disks::thread_read_bytes()) {
                            (Some(a), Some(b)) => Some(b.saturating_sub(a)),
                            _ => None,
                        };
                        Outcome {
                            target: job.target,
                            state: job.state,
                            result,
                            reads: after.saturating_sub(before),
                            counter,
                            own_bytes,
                        }
                    })
                })
                .collect();
            handles.into_iter().filter_map(|h| h.join().ok()).collect()
        });

        let t_done = now();
        for o in outcomes {
            let ts = per.entry(o.target.mount.clone()).or_default();
            let first = !primed.get(&o.target.mount).copied().unwrap_or(false);
            ts.entries = o.result.entries();
            ts.errors = o.result.errors;
            ts.last_refresh = t_done;
            ts.last_ms = o.result.elapsed.as_millis() as u64;
            ts.last_reads = o.reads;
            ts.last_counter = Some(o.counter);
            ts.top = o.result.top.clone();

            if first {
                info!(
                    "{}: primed {} entries ({} folders, {} files) in {:.1}s, {} reads",
                    o.target.name,
                    o.result.entries(),
                    o.result.dirs,
                    o.result.files,
                    o.result.elapsed.as_secs_f64(),
                    o.reads
                );
                primed.insert(o.target.mount.clone(), true);
            } else if o.state == Some(true) && caused_reads(&o) == Some(true) {
                ts.wakes += 1;
                ts.last_wake = t_done;
                ts.backoff_secs = (ts.backoff_secs * 2).clamp(BACKOFF_MIN, BACKOFF_MAX);
                ts.backoff_until = t_done + ts.backoff_secs;
                warn!(
                    "{}: cached metadata was evicted while the disk slept; the refresh had to read {} from disk and took {:.1}s, \
                     which woke it. Not refreshing it while asleep for {}h. If this repeats: add RAM, lower cache pressure, or exclude large folders.",
                    o.target.name,
                    human_bytes(o.own_bytes.unwrap_or(0)),
                    o.result.elapsed.as_secs_f64(),
                    ts.backoff_secs / 3600
                );
            } else if o.state == Some(true) {
                // a clean refresh while asleep: the cache is holding, reset back-off
                ts.backoff_secs = 0;
                debug!("{}: refreshed while asleep, 0 reads ({} entries)", o.target.name, o.result.entries());
            } else if o.own_bytes.map_or(o.reads > 0, |n| n > 0) {
                ts.refills += 1;
                match o.own_bytes {
                    Some(n) => debug!("{}: refilled cache while awake (read {} from disk)", o.target.name, human_bytes(n)),
                    None => debug!("{}: refilled cache while awake (disk did {} reads)", o.target.name, o.reads),
                }
            }
        }

        st.cache_bytes = disks::metadata_cache_bytes();
        if st.mem_total > 0 && st.cache_bytes * 100 > st.mem_total * cfg.budget_pct && t_now > last_budget_warn + 3600 {
            warn!(
                "metadata cache uses {} ({}% of RAM), above the {}% budget. Consider excluding folders with many small files.",
                human_bytes(st.cache_bytes),
                st.cache_bytes * 100 / st.mem_total,
                cfg.budget_pct
            );
            last_budget_warn = t_now;
        }

        let mut folders: Vec<String> = per.values().filter(|t| t.seen).flat_map(|t| t.top.iter().cloned()).collect();
        folders.sort_by_key(|f| f.to_lowercase());
        folders.dedup();
        st.folders = folders;

        st.last_cycle = t_done;
        st.targets = per.values().cloned().collect();
        sort_targets(&mut st.targets);
        st.write(status_dir);

        if once {
            return;
        }
        thread::sleep(Duration::from_secs(cfg.interval));
    }
}

/// Can a wake on this target be attributed exactly? ZFS reads from disk in its
/// own kernel threads, so per-thread accounting can't see them; guessing from the
/// disk's counter would blame us for other programs' reads.
fn wake_detectable(fs: &str) -> bool {
    fs != "zfs" && disks::thread_read_bytes().is_some()
}

/// Did this refresh itself read from the disk? Only the walking thread's own I/O
/// counts, so a program reading the same disk at the same moment (a media server
/// starting playback) is never blamed on us. None = can't be measured exactly.
fn caused_reads(o: &Outcome) -> Option<bool> {
    if o.target.fs == "zfs" {
        return None;
    }
    o.own_bytes.map(|n| n > 0)
}

fn sort_targets(v: &mut [TargetStatus]) {
    fn key(t: &TargetStatus) -> (u8, String, u64) {
        let k = match t.kind.as_str() {
            "array" => 0,
            "pool" => 1,
            _ => 2,
        };
        let digits: String = t.name.chars().filter(|c| c.is_ascii_digit()).collect();
        let prefix: String = t.name.chars().filter(|c| !c.is_ascii_digit()).collect();
        (k, prefix, digits.parse().unwrap_or(0))
    }
    v.sort_by_key(key);
}
