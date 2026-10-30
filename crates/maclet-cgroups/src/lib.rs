use maclet_common::{CgroupMetrics, ResourceLimits, Result};
#[cfg(target_os = "linux")]
use maclet_common::MacletError;
use std::fs;
#[cfg(target_os = "linux")]
use std::path::Path;
use std::path::PathBuf;

pub struct CgroupV2Manager {
    root: PathBuf,
}

impl Default for CgroupV2Manager {
    fn default() -> Self {
        Self::new()
    }
}

impl CgroupV2Manager {
    pub fn new() -> Self {
        Self {
            root: PathBuf::from("/sys/fs/cgroup"),
        }
    }

    /// Initializes cgroups v2 hierarchy and enables controllers
    pub fn init(&self) -> Result<()> {
        #[cfg(target_os = "linux")]
        {
            self.ensure_mounted()?;
            self.enable_controllers(&self.root)?;

            let maclet_root = self.root.join("maclet");
            fs::create_dir_all(&maclet_root)?;
            self.enable_controllers(&maclet_root)?;
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn ensure_mounted(&self) -> Result<()> {
        if !self.root.join("cgroup.controllers").exists() {
            fs::create_dir_all(&self.root)?;
            unsafe {
                use std::ffi::CString;
                let cgroup_type = CString::new("cgroup2").unwrap();
                let target_c = CString::new(self.root.to_str().unwrap()).unwrap();
                let ret = libc::mount(
                    cgroup_type.as_ptr(),
                    target_c.as_ptr(),
                    cgroup_type.as_ptr(),
                    0,
                    std::ptr::null(),
                );
                if ret != 0 {
                    return Err(MacletError::Guest(format!(
                        "Failed to mount cgroup2 on {}: {}",
                        self.root.display(),
                        std::io::Error::last_os_error()
                    )));
                }
            }
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn enable_controllers(&self, path: &Path) -> Result<()> {
        let subtree_file = path.join("cgroup.subtree_control");
        if subtree_file.exists() {
            // Enable cpu, memory, pids if supported
            let _ = fs::write(&subtree_file, "+cpu +memory +pids\n");
        }
        Ok(())
    }

    /// Creates a cgroup for a container and applies resource limits
    pub fn create_container_cgroup(
        &self,
        id: &str,
        limits: &Option<ResourceLimits>,
    ) -> Result<PathBuf> {
        let cgroup_dir = self.root.join("maclet").join(id);
        fs::create_dir_all(&cgroup_dir)?;

        if let Some(lim) = limits {
            // 1. Memory limit
            if let Some(bytes) = lim.memory_bytes {
                let mem_max_file = cgroup_dir.join("memory.max");
                let _ = fs::write(mem_max_file, format!("{}\n", bytes));
            }

            // 2. CPU limit
            if let (Some(quota), Some(period)) = (lim.cpu_quota_usec, lim.cpu_period_usec) {
                let cpu_max_file = cgroup_dir.join("cpu.max");
                let _ = fs::write(cpu_max_file, format!("{} {}\n", quota, period));
            }

            // 3. Process count limit
            if let Some(pids) = lim.pids_max {
                let pids_max_file = cgroup_dir.join("pids.max");
                let _ = fs::write(pids_max_file, format!("{}\n", pids));
            }
        }

        Ok(cgroup_dir)
    }

    /// Attaches a process to the container cgroup (0 moves current process)
    pub fn attach_process(&self, id: &str, pid: i32) -> Result<()> {
        let procs_file = self.root.join("maclet").join(id).join("cgroup.procs");
        if procs_file.exists() {
            fs::write(procs_file, format!("{}\n", pid))?;
        }
        Ok(())
    }

    /// Inspects live resource metrics of a container
    pub fn read_metrics(&self, id: &str) -> Result<CgroupMetrics> {
        let cgroup_dir = self.root.join("maclet").join(id);
        let mut metrics = CgroupMetrics::default();

        // Memory current
        if let Ok(content) = fs::read_to_string(cgroup_dir.join("memory.current")) {
            metrics.memory_current = content.trim().parse().unwrap_or(0);
        }

        // Memory max
        if let Ok(content) = fs::read_to_string(cgroup_dir.join("memory.max")) {
            let trimmed = content.trim();
            if trimmed != "max" {
                metrics.memory_max = trimmed.parse().ok();
            }
        }

        // PIDs current
        if let Ok(content) = fs::read_to_string(cgroup_dir.join("pids.current")) {
            metrics.pids_current = content.trim().parse().unwrap_or(0);
        }

        // PIDs max
        if let Ok(content) = fs::read_to_string(cgroup_dir.join("pids.max")) {
            let trimmed = content.trim();
            if trimmed != "max" {
                metrics.pids_max = trimmed.parse().ok();
            }
        }

        // CPU stats
        if let Ok(content) = fs::read_to_string(cgroup_dir.join("cpu.stat")) {
            for line in content.lines() {
                let mut parts = line.split_whitespace();
                if let (Some(key), Some(val)) = (parts.next(), parts.next()) {
                    match key {
                        "usage_usec" => metrics.cpu_usage_usec = val.parse().unwrap_or(0),
                        "user_usec" => metrics.cpu_user_usec = val.parse().unwrap_or(0),
                        "system_usec" => metrics.cpu_system_usec = val.parse().unwrap_or(0),
                        _ => {}
                    }
                }
            }
        }

        Ok(metrics)
    }

    /// Cleans up and removes a container cgroup directory
    pub fn destroy_container_cgroup(&self, id: &str) {
        let cgroup_dir = self.root.join("maclet").join(id);
        let _ = fs::remove_dir(&cgroup_dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cgroup_manager_creation() {
        let mgr = CgroupV2Manager::new();
        assert_eq!(mgr.root, PathBuf::from("/sys/fs/cgroup"));
    }
}
