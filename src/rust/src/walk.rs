//! The walker. Unlike `find` with no tests, it stats every entry (not just
//! directories), so file inodes stay cached too. That is what SMB, `ls -l` and the
//! Unraid file browser need when they list a folder.

use std::collections::HashSet;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Default)]
pub struct WalkResult {
    pub dirs: u64,
    pub files: u64,
    pub errors: u64,
    pub elapsed: Duration,
    /// every top-level folder on the target, before include/exclude filtering
    pub top: Vec<String>,
}

impl WalkResult {
    pub fn entries(&self) -> u64 {
        self.dirs + self.files
    }
}

/// Walks `root`. Top-level folders are filtered by `include` (empty = all) and
/// `exclude`. `max_depth` 0 = unlimited; 1 = only the share folders themselves.
/// Never follows symlinks. Descends into child filesystems that belong to the same
/// disk (ZFS datasets, btrfs subvolumes) but not into mounts listed in `foreign`
/// (another disk, a network share, ...).
pub fn walk(
    root: &Path,
    include: &[String],
    exclude: &[String],
    max_depth: usize,
    foreign: &HashSet<PathBuf>,
) -> WalkResult {
    let start = Instant::now();
    let mut r = WalkResult::default();
    let root_dev = match fs::symlink_metadata(root) {
        Ok(m) => m.dev(),
        Err(_) => {
            r.errors += 1;
            r.elapsed = start.elapsed();
            return r;
        }
    };

    // a different st_dev means a mount point or a btrfs subvolume; only real
    // foreign mounts are skipped
    let enter = |path: &Path, dev: u64, parent_dev: u64| dev == parent_dev || !foreign.contains(path);

    let mut stack: Vec<(PathBuf, usize, u64)> = Vec::new();
    match fs::read_dir(root) {
        Ok(rd) => {
            r.dirs += 1;
            for e in rd {
                let e = match e {
                    Ok(e) => e,
                    Err(_) => {
                        r.errors += 1;
                        continue;
                    }
                };
                let name = e.file_name().to_string_lossy().into_owned();
                let md = match e.metadata() {
                    Ok(m) => m,
                    Err(_) => {
                        r.errors += 1;
                        continue;
                    }
                };
                if md.is_dir() {
                    r.top.push(name.clone());
                    let wanted = (include.is_empty() || include.iter().any(|i| i == &name))
                        && !exclude.iter().any(|x| x == &name);
                    let path = e.path();
                    if wanted && enter(&path, md.dev(), root_dev) {
                        stack.push((path, 1, md.dev()));
                    }
                } else {
                    r.files += 1;
                }
            }
        }
        Err(_) => r.errors += 1,
    }

    while let Some((dir, depth, dir_dev)) = stack.pop() {
        let rd = match fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(_) => {
                r.errors += 1;
                continue;
            }
        };
        r.dirs += 1;
        for e in rd {
            let e = match e {
                Ok(e) => e,
                Err(_) => {
                    r.errors += 1;
                    continue;
                }
            };
            // DirEntry::metadata does not follow symlinks; this is the stat that
            // pulls the inode into the cache.
            let md = match e.metadata() {
                Ok(m) => m,
                Err(_) => {
                    r.errors += 1;
                    continue;
                }
            };
            if md.is_dir() {
                let path = e.path();
                if (max_depth == 0 || depth < max_depth) && enter(&path, md.dev(), dir_dev) {
                    stack.push((path, depth + 1, md.dev()));
                } else {
                    r.dirs += 1;
                }
            } else {
                r.files += 1;
            }
        }
    }
    r.elapsed = start.elapsed();
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{create_dir_all, File};

    #[test]
    fn walks_with_filters_and_depth() {
        let base = std::env::temp_dir().join(format!("cd2test{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        create_dir_all(base.join("Media/Movies/A")).unwrap();
        create_dir_all(base.join("appdata/x")).unwrap();
        create_dir_all(base.join("My Photos")).unwrap();
        File::create(base.join("Media/Movies/A/a.mkv")).unwrap();
        File::create(base.join("Media/m.txt")).unwrap();
        File::create(base.join("appdata/x/db")).unwrap();
        File::create(base.join("My Photos/p.jpg")).unwrap();

        let none = HashSet::new();
        let all = walk(&base, &[], &[], 0, &none);
        let mut top = all.top.clone();
        top.sort();
        assert_eq!(top, vec!["Media", "My Photos", "appdata"]);
        // dirs: root, Media, Movies, A, appdata, x, My Photos = 7 ; files 4
        assert_eq!((all.dirs, all.files), (7, 4));

        let ex = walk(&base, &[], &["appdata".to_string()], 0, &none);
        assert_eq!(ex.top.len(), 3); // listed even when excluded
        assert_eq!((ex.dirs, ex.files), (5, 3));

        let inc = walk(&base, &["My Photos".to_string()], &[], 0, &none);
        assert_eq!((inc.dirs, inc.files), (2, 1));

        // depth 1: Media is listed, Movies is stat'd but not entered
        let d1 = walk(&base, &["Media".to_string()], &[], 1, &none);
        assert_eq!((d1.dirs, d1.files), (3, 1));
        fs::remove_dir_all(&base).unwrap();
    }
}
