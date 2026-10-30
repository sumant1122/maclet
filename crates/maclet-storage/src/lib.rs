use maclet_common::Result;
use std::fs;
use std::path::PathBuf;

#[cfg(target_os = "linux")]
use maclet_common::MacletError;
#[cfg(target_os = "linux")]
use std::ffi::CString;

pub struct OverlayStorage {
    base_dir: PathBuf,
}

impl Default for OverlayStorage {
    fn default() -> Self {
        Self::new(PathBuf::from("/var/lib/maclet"))
    }
}

impl OverlayStorage {
    pub fn new(base_dir: PathBuf) -> Self {
        Self { base_dir }
    }

    pub fn layers_dir(&self) -> PathBuf {
        self.base_dir.join("layers")
    }

    pub fn containers_dir(&self) -> PathBuf {
        self.base_dir.join("containers")
    }

    pub fn container_rootfs(&self, container_id: &str) -> PathBuf {
        self.containers_dir().join(container_id).join("rootfs")
    }

    /// Prepares a container's rootfs using genuine Linux OverlayFS
    pub fn prepare_container_rootfs(
        &self,
        container_id: &str,
        lower_layers: &[PathBuf],
    ) -> Result<PathBuf> {
        let cnt_dir = self.containers_dir().join(container_id);
        let upper = cnt_dir.join("upper");
        let work = cnt_dir.join("work");
        let merged = cnt_dir.join("rootfs");

        fs::create_dir_all(&upper)?;
        fs::create_dir_all(&work)?;
        fs::create_dir_all(&merged)?;

        #[cfg(not(target_os = "linux"))]
        let _ = lower_layers;

        #[cfg(target_os = "linux")]
        {
            let lower_str = if lower_layers.is_empty() {
                // Fallback to base Alpine rootfs if no custom image layers specified
                let base = self.base_dir.join("rootfs");
                base.to_string_lossy().to_string()
            } else {
                lower_layers
                    .iter()
                    .map(|p| p.to_string_lossy().to_string())
                    .collect::<Vec<_>>()
                    .join(":")
            };

            let mount_opts = format!(
                "lowerdir={},upperdir={},workdir={}",
                lower_str,
                upper.to_string_lossy(),
                work.to_string_lossy()
            );

            let overlay_type = CString::new("overlay").unwrap();
            let merged_c = CString::new(merged.to_str().unwrap()).unwrap();
            let opts_c = CString::new(mount_opts.as_str()).unwrap();

            let ret = unsafe {
                libc::mount(
                    overlay_type.as_ptr(),
                    merged_c.as_ptr(),
                    overlay_type.as_ptr(),
                    0,
                    opts_c.as_ptr() as *const libc::c_void,
                )
            };

            if ret != 0 {
                // If OverlayFS mount fails, fall back to recursive bind mount
                let err = std::io::Error::last_os_error();
                let src_c = CString::new(
                    lower_layers
                        .first()
                        .unwrap_or(&self.base_dir.join("rootfs"))
                        .to_str()
                        .unwrap(),
                )
                .unwrap();
                let bind_ret = unsafe {
                    libc::mount(
                        src_c.as_ptr(),
                        merged_c.as_ptr(),
                        std::ptr::null(),
                        libc::MS_BIND | libc::MS_REC,
                        std::ptr::null(),
                    )
                };
                if bind_ret != 0 {
                    return Err(MacletError::Guest(format!(
                        "Failed to mount rootfs (overlay err: {}, bind err: {})",
                        err,
                        std::io::Error::last_os_error()
                    )));
                }
            }
        }

        Ok(merged)
    }

    /// Safely unmounts and cleans up a container's OverlayFS root
    pub fn cleanup_container_rootfs(&self, container_id: &str) -> Result<()> {
        let cnt_dir = self.containers_dir().join(container_id);
        let merged = cnt_dir.join("rootfs");

        #[cfg(target_os = "linux")]
        {
            if let Ok(merged_c) = CString::new(merged.to_str().unwrap()) {
                unsafe {
                    libc::umount2(merged_c.as_ptr(), libc::MNT_DETACH);
                }
            }
        }
        #[cfg(not(target_os = "linux"))]
        let _ = merged;

        let _ = fs::remove_dir_all(&cnt_dir);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_storage_paths() {
        let storage = OverlayStorage::new(PathBuf::from("/tmp/maclet"));
        assert_eq!(storage.layers_dir(), PathBuf::from("/tmp/maclet/layers"));
        assert_eq!(
            storage.container_rootfs("c1"),
            PathBuf::from("/tmp/maclet/containers/c1/rootfs")
        );
    }
}
