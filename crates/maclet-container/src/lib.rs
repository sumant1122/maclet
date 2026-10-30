use maclet_cgroups::CgroupV2Manager;
use maclet_common::{CgroupMetrics, ResourceLimits, Result};
use maclet_namespaces::{
    run_in_isolated_namespaces, spawn_isolated_namespaces, ContainerConfig, ExecutionResult,
};
use maclet_storage::OverlayStorage;
use std::collections::HashMap;
use std::net::Ipv4Addr;
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
    pub ports: Vec<String>,
    pub read_only: bool,
    pub drop_caps: Vec<String>,
    pub assigned_ip: Option<Ipv4Addr>,
    cgroup_mgr: CgroupV2Manager,
}

impl Container {
    pub fn new(
        id: String,
        base_rootfs: &Path,
        command: String,
        args: Vec<String>,
        env: HashMap<String, String>,
        custom_hostname: Option<String>,
        limits: Option<ResourceLimits>,
        ports: Vec<String>,
        read_only: bool,
        drop_caps: Vec<String>,
        assigned_ip: Option<Ipv4Addr>,
    ) -> Result<Self> {
        let hostname = custom_hostname.unwrap_or_else(|| {
            let short = if id.len() > 12 { &id[..12] } else { &id };
            format!("maclet-{}", short)
        });

        // Initialize OverlayFS rootfs for container
        let storage = OverlayStorage::new(PathBuf::from("/var/lib/maclet"));
        let rootfs = storage.prepare_container_rootfs(&id, &[base_rootfs.to_path_buf()])?;

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
            ports,
            read_only,
            drop_caps,
            assigned_ip,
            cgroup_mgr,
        })
    }

    pub fn run(&self) -> Result<(ExecutionResult, Option<CgroupMetrics>)> {
        let netns_enabled = self.assigned_ip.is_some() || !self.ports.is_empty();
        let config = ContainerConfig {
            id: self.id.clone(),
            hostname: self.hostname.clone(),
            rootfs: self.rootfs.clone(),
            command: self.command.clone(),
            args: self.args.clone(),
            env: self.env.clone(),
            cgroup_path: self.cgroup_path.clone(),
            netns_enabled,
            assigned_ip: self.assigned_ip,
            ports: self.ports.clone(),
            read_only: self.read_only,
            drop_caps: self.drop_caps.clone(),
        };

        let exec_result = run_in_isolated_namespaces(&config)?;
        let metrics = self.cgroup_mgr.read_metrics(&self.id).ok();

        Ok((exec_result, metrics))
    }

    pub fn spawn_detached(&self, log_path: &Path, exit_file: &Path) -> Result<(u32, u32)> {
        let netns_enabled = self.assigned_ip.is_some() || !self.ports.is_empty();
        let config = ContainerConfig {
            id: self.id.clone(),
            hostname: self.hostname.clone(),
            rootfs: self.rootfs.clone(),
            command: self.command.clone(),
            args: self.args.clone(),
            env: self.env.clone(),
            cgroup_path: self.cgroup_path.clone(),
            netns_enabled,
            assigned_ip: self.assigned_ip,
            ports: self.ports.clone(),
            read_only: self.read_only,
            drop_caps: self.drop_caps.clone(),
        };

        spawn_isolated_namespaces(&config, log_path, exit_file)
    }

    pub fn cleanup(&self) {
        let storage = OverlayStorage::new(PathBuf::from("/var/lib/maclet"));
        let _ = storage.cleanup_container_rootfs(&self.id);
        self.cgroup_mgr.destroy_container_cgroup(&self.id);
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
            ports: vec!["8080:80".to_string()],
            read_only: false,
            drop_caps: vec![],
            assigned_ip: Some(Ipv4Addr::new(10, 88, 0, 5)),
            cgroup_mgr: CgroupV2Manager::new(),
        };
        assert_eq!(cnt.id, "c1");
        assert_eq!(cnt.ports, vec!["8080:80"]);
    }
}
