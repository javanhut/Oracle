//! Storage: where the space went.
//!
//! "No space left on device" is the single most common cause of a machine that
//! has started behaving strangely in several unrelated ways at once, and it is
//! nearly always invisible until something refuses to start. Package installs
//! fail, logs stop, the session will not save its settings, and none of those
//! symptoms mention the disk.

use serde::Serialize;

#[derive(Debug, Default, Serialize)]
pub struct Storage {
    pub filesystems: Vec<Filesystem>,
    /// Size of the Raven log directory, when there is one. Logs are the usual
    /// way a root filesystem fills up quietly.
    pub log_dir_bytes: Option<u64>,
    pub log_dir: Option<String>,
    /// Size of the package manager's download cache.
    pub package_cache_bytes: Option<u64>,
    pub package_cache: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Filesystem {
    pub mount: String,
    pub device: String,
    pub fstype: String,
    pub total_bytes: u64,
    pub available_bytes: u64,
    pub used_percent: u8,
    /// Percentage of inodes used. A filesystem can be out of inodes with
    /// plenty of bytes free, and the error message is the same "no space".
    pub inodes_used_percent: Option<u8>,
    pub read_only: bool,
}

impl Filesystem {
    pub fn used_bytes(&self) -> u64 {
        self.total_bytes.saturating_sub(self.available_bytes)
    }
}

/// Filesystem types that represent real storage. Everything else -- tmpfs,
/// proc, sysfs, cgroup, the squashfs layers of a live image -- either cannot
/// fill up in a way the user can act on, or filling up is normal.
const REAL_FS: [&str; 13] = [
    "ext2", "ext3", "ext4", "btrfs", "xfs", "f2fs", "vfat", "exfat", "ntfs", "ntfs3", "zfs", "jfs",
    "reiserfs",
];

pub fn probe() -> Storage {
    let mut s = Storage {
        filesystems: mounted_filesystems(),
        ..Default::default()
    };

    if std::path::Path::new("/var/log/raven").is_dir() {
        s.log_dir = Some("/var/log/raven".into());
        s.log_dir_bytes = dir_size("/var/log/raven");
    }

    for cache in ["/var/cache/rvn", "/var/cache/pacman/pkg"] {
        if std::path::Path::new(cache).is_dir() {
            s.package_cache = Some(cache.into());
            s.package_cache_bytes = dir_size(cache);
            break;
        }
    }

    s
}

fn mounted_filesystems() -> Vec<Filesystem> {
    let Some(mounts) = crate::sys::read("/proc/mounts") else {
        return Vec::new();
    };

    let inode_map = inode_usage();
    let mut out: Vec<Filesystem> = Vec::new();

    for line in mounts.lines() {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 4 {
            continue;
        }
        let (device, mount, fstype, opts) = (cols[0], cols[1], cols[2], cols[3]);
        if !REAL_FS.contains(&fstype) {
            continue;
        }
        // /proc/mounts escapes spaces in mount points as \040.
        let mount = mount.replace("\\040", " ");

        // A bind mount reports the same device and usage twice; once is enough.
        if out.iter().any(|f| f.device == device && f.fstype == fstype) {
            continue;
        }

        let Some((available, total)) = crate::sys::disk_usage(&mount) else {
            continue;
        };
        if total == 0 {
            continue;
        }

        let used = total.saturating_sub(available);
        let used_percent = ((used as f64 / total as f64) * 100.0).round() as u8;

        out.push(Filesystem {
            device: device.to_string(),
            fstype: fstype.to_string(),
            total_bytes: total,
            available_bytes: available,
            used_percent,
            inodes_used_percent: inode_map.iter().find(|(m, _)| m == &mount).map(|(_, p)| *p),
            read_only: opts.split(',').any(|o| o == "ro"),
            mount,
        });
    }

    out.sort_by_key(|f| std::cmp::Reverse(f.used_percent));
    out
}

/// Inode usage per mount point, from a single `df -Pi`.
fn inode_usage() -> Vec<(String, u8)> {
    let Some(out) = crate::sys::quick("df", &["-Pi"]) else {
        return Vec::new();
    };
    let Some(text) = out.text() else {
        return Vec::new();
    };
    let mut v = Vec::new();
    for line in text.lines().skip(1) {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 6 {
            continue;
        }
        // Filesystem Inodes IUsed IFree IUse% Mounted-on
        if let Ok(pct) = cols[4].trim_end_matches('%').parse::<u8>() {
            v.push((cols[5..].join(" "), pct));
        }
    }
    v
}

/// Total size of a directory tree, via `du`.
///
/// Bounded by the two-second command timeout: on a pathological tree this
/// returns `None` rather than making `oracle doctor` feel slow, and a missing
/// cache size costs the report nothing.
fn dir_size(path: &str) -> Option<u64> {
    let out = crate::sys::run(
        "du",
        &["-sk", "--", path],
        std::time::Duration::from_secs(3),
    )?;
    let text = out.text()?;
    let kb: u64 = text.split_whitespace().next()?.parse().ok()?;
    Some(kb * 1024)
}

impl Storage {
    /// Filesystems at or above `pct` used.
    pub fn tight(&self, pct: u8) -> Vec<&Filesystem> {
        self.filesystems
            .iter()
            .filter(|f| f.used_percent >= pct)
            .collect()
    }

    pub fn read_only(&self) -> Vec<&Filesystem> {
        self.filesystems.iter().filter(|f| f.read_only).collect()
    }

    pub fn root(&self) -> Option<&Filesystem> {
        self.filesystems.iter().find(|f| f.mount == "/")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fs(mount: &str, used: u8, ro: bool) -> Filesystem {
        Filesystem {
            mount: mount.into(),
            device: "/dev/sda1".into(),
            fstype: "ext4".into(),
            total_bytes: 100 * 1024 * 1024 * 1024,
            available_bytes: (100 - used as u64) * 1024 * 1024 * 1024,
            used_percent: used,
            inodes_used_percent: None,
            read_only: ro,
        }
    }

    #[test]
    fn tight_filters_by_threshold() {
        let s = Storage {
            filesystems: vec![fs("/", 95, false), fs("/home", 40, false)],
            ..Default::default()
        };
        assert_eq!(s.tight(90).len(), 1);
        assert_eq!(s.tight(90)[0].mount, "/");
        assert_eq!(s.tight(30).len(), 2);
    }

    #[test]
    fn a_read_only_root_is_found() {
        let s = Storage {
            filesystems: vec![fs("/", 50, true)],
            ..Default::default()
        };
        assert_eq!(s.read_only().len(), 1);
    }

    #[test]
    fn used_bytes_never_underflows() {
        let mut f = fs("/", 10, false);
        f.available_bytes = f.total_bytes + 1;
        assert_eq!(f.used_bytes(), 0);
    }

    #[test]
    fn the_real_machine_has_a_root_filesystem() {
        let s = probe();
        assert!(
            s.root().is_some(),
            "every Linux mounts something real at /; got {:?}",
            s.filesystems
        );
    }
}
