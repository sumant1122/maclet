use maclet_cgroups::CgroupV2Manager;
use maclet_common::{CgroupMetrics, ResourceLimits, Result};
use maclet_namespaces::{
    run_in_isolated_namespaces, spawn_isolated_namespaces, ContainerConfig, ExecutionResult,
};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

pub struct Container {
    pub id: String,
    pub hostname: String,
    pub rootfs: PathBuf,
    pub command: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub limits: Option<ResourceLimits>,
    pub cgroup_path: Option<PathBuf>,
    cgroup_mgr: CgroupV2Manager,
}

impl Container {
    pub fn new(
        id: String,
        #[allow(unused_variables)] base_rootfs: &Path,
        command: String,
        args: Vec<String>,
        env: HashMap<String, String>,
        custom_hostname: Option<String>,
        limits: Option<ResourceLimits>,
    ) -> Result<Self> {
        let hostname = custom_hostname.unwrap_or_else(|| {
            let short = if id.len() > 12 { &id[..12] } else { &id };
            format!("maclet-{}", short)
        });

        // Ensure container private directory exists
        let container_root = PathBuf::from("/var/lib/maclet/containers").join(&id);
        let rootfs = container_root.join("rootfs");
        fs::create_dir_all(&rootfs)?;

        #[cfg(target_os = "linux")]
        if base_rootfs.exists() {
            unsafe {
                use std::ffi::CString;
                let src_c = CString::new(base_rootfs.to_str().unwrap()).unwrap();
                let dst_c = CString::new(rootfs.to_str().unwrap()).unwrap();
                let none: *const libc::c_char = std::ptr::null();
                libc::mount(
                    src_c.as_ptr(),
                    dst_c.as_ptr(),
                    none,
                    libc::MS_BIND | libc::MS_REC,
                    std::ptr::null(),
                );
            }
        }

        // Initialize cgroups v2
        let cgroup_mgr = CgroupV2Manager::new();
        let _ = cgroup_mgr.init();
        let cgroup_path = cgroup_mgr.create_container_cgroup(&id, &limits).ok();

        Ok(Self {
            id,
            hostname,
            rootfs,
            command,
            args,
            env,
            limits,
            cgroup_path,
            cgroup_mgr,
        })
    }

    pub fn run(&self) -> Result<(ExecutionResult, Option<CgroupMetrics>)> {
        let config = ContainerConfig {
            id: self.id.clone(),
            hostname: self.hostname.clone(),
            rootfs: self.rootfs.clone(),
            command: self.command.clone(),
            args: self.args.clone(),
            env: self.env.clone(),
            cgroup_path: self.cgroup_path.clone(),
        };

        let exec_result = run_in_isolated_namespaces(&config)?;
        let metrics = self.cgroup_mgr.read_metrics(&self.id).ok();

        Ok((exec_result, metrics))
    }

    pub fn spawn_detached(&self, log_path: &Path, exit_file: &Path) -> Result<(u32, u32)> {
        let config = ContainerConfig {
            id: self.id.clone(),
            hostname: self.hostname.clone(),
            rootfs: self.rootfs.clone(),
            command: self.command.clone(),
            args: self.args.clone(),
            env: self.env.clone(),
            cgroup_path: self.cgroup_path.clone(),
        };

        spawn_isolated_namespaces(&config, log_path, exit_file)
    }

    pub fn cleanup(&self) {
        #[cfg(target_os = "linux")]
        unsafe {
            use std::ffi::CString;
            if let Ok(dst_c) = CString::new(self.rootfs.to_str().unwrap()) {
                libc::umount2(dst_c.as_ptr(), libc::MNT_DETACH);
            }
        }
        self.cgroup_mgr.destroy_container_cgroup(&self.id);
        let container_root = PathBuf::from("/var/lib/maclet/containers").join(&self.id);
        let _ = fs::remove_dir_all(&container_root);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_container_struct() {
        let cnt = Container {
            id: "c1".to_string(),
            hostname: "maclet-c1".to_string(),
            rootfs: PathBuf::from("/tmp/c1"),
            command: "/bin/sh".to_string(),
            args: vec![],
            env: HashMap::new(),
            limits: None,
            cgroup_path: None,
            cgroup_mgr: CgroupV2Manager::new(),
        };
        assert_eq!(cnt.id, "c1");
    }
}
