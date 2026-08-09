//! Host resource metrics: CPU/RAM/disk for status reporting and apply-time
//! budget checks. Thin, `cfg`-gated readers; no unsafe in this module
//! (`statvfs` goes through the safe `nix` wrapper).

use std::path::Path;

/// Logical CPUs on this host.
pub fn host_cpus() -> u32 {
    num_cpus::get() as u32
}

/// Total physical memory MiB (0 when the platform is not detected).
pub fn host_memory_mib() -> u64 {
    #[cfg(target_os = "linux")]
    {
        if let Ok(meminfo) = std::fs::read_to_string("/proc/meminfo") {
            for line in meminfo.lines() {
                if let Some(rest) = line.strip_prefix("MemTotal:") {
                    let kb = rest.trim().trim_end_matches("kB").trim();
                    if let Ok(kb) = kb.parse::<u64>() {
                        return kb / 1024;
                    }
                }
            }
        }
        0
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("sysctl")
            .args(["-n", "hw.memsize"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .and_then(|s| s.trim().parse::<u64>().ok())
            .map(|bytes| bytes / (1024 * 1024))
            .unwrap_or(0)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        0
    }
}

/// (total MiB, free MiB) on the filesystem containing `path`.
pub fn disk_usage_mib(path: &Path) -> (u64, u64) {
    use nix::sys::statvfs::statvfs;
    let Ok(vfs) = statvfs(path) else {
        return (0, 0);
    };
    let frsize = if vfs.fragment_size() != 0 {
        vfs.fragment_size()
    } else {
        vfs.block_size()
    };
    let total = (vfs.blocks() as u64).saturating_mul(frsize) / (1024 * 1024);
    let free = (vfs.blocks_available() as u64).saturating_mul(frsize) / (1024 * 1024);
    (total, free)
}

/// Recursive on-disk size of `path` in bytes (real directories only; symlinked
/// directories are not followed, so symlink cycles cannot loop).
pub fn dir_size_bytes(path: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            if ft.is_dir() {
                stack.push(entry.path());
            } else {
                total = total.saturating_add(entry.metadata().map(|m| m.len()).unwrap_or(0));
            }
        }
    }
    total
}

/// Recursive on-disk size of `path` in MiB.
pub fn dir_size_mib(path: &Path) -> u64 {
    dir_size_bytes(path) / (1024 * 1024)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_size_counts_files_recursively() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("a/b")).unwrap();
        std::fs::write(dir.path().join("one"), vec![1u8; 2048]).unwrap();
        std::fs::write(dir.path().join("a/two"), vec![2u8; 1024]).unwrap();
        std::fs::write(dir.path().join("a/b/three"), vec![3u8; 4096]).unwrap();
        assert_eq!(dir_size_bytes(dir.path()), 2048 + 1024 + 4096);
    }

    #[test]
    fn disk_usage_reads_filesystem() {
        let (total, free) = disk_usage_mib(std::path::Path::new("/"));
        assert!(total > 0, "total disk should be non-zero");
        assert!(free <= total, "free ({free}) cannot exceed total ({total})");
    }
}
