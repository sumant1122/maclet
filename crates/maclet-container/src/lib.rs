use maclet_common::Result;
use maclet_namespaces::{run_in_isolated_namespaces, ContainerConfig, ExecutionResult};
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
}

impl Container {
    pub fn new(
        id: String,
        #[allow(unused_variables)] base_rootfs: &Path,
        command: String,
        args: Vec<String>,
        env: HashMap<String, String>,
        custom_hostname: Option<String>,
    ) -> Result<Self> {
        let hostname = custom_hostname.unwrap_or_else(|| {
            let short = if id.len() > 12 { &id[..12] } else { &id };
            format!("maclet-{}", short)
        });

        // Ensure container private directory exists
        let container_root = PathBuf::from("/var/lib/maclet/containers").join(&id);
        let rootfs = container_root.join("rootfs");
        fs::create_dir_all(&rootfs)?;

        // Prepare rootfs: if base_rootfs exists, populate or bind
        // In Phase 2, we copy or bind mount the base rootfs
        #[cfg(target_os = "linux")]
        if base_rootfs.exists() {
            // Bind mount base_rootfs into container rootfs
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

        Ok(Self {
            id,
            hostname,
            rootfs,
            command,
            args,
            env,
        })
    }

    pub fn run(&self) -> Result<ExecutionResult> {
        let config = ContainerConfig {
            id: self.id.clone(),
            hostname: self.hostname.clone(),
            rootfs: self.rootfs.clone(),
            command: self.command.clone(),
            args: self.args.clone(),
            env: self.env.clone(),
        };

        run_in_isolated_namespaces(&config)
    }

    pub fn cleanup(&self) {
        #[cfg(target_os = "linux")]
        unsafe {
            use std::ffi::CString;
            if let Ok(dst_c) = CString::new(self.rootfs.to_str().unwrap()) {
                libc::umount2(dst_c.as_ptr(), libc::MNT_DETACH);
            }
        }
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
        };
        assert_eq!(cnt.id, "c1");
    }
}
