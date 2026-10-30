pub mod ffi;

use ffi::{VzConfig, VzVmPtr};
use maclet_common::{MacletError, Result, VmConfig};
use std::ffi::{CStr, CString};
use std::os::unix::io::IntoRawFd;
use std::os::unix::net::UnixStream as StdUnixStream;
use tokio::net::UnixStream;

pub struct VirtualMachine {
    vm_ptr: VzVmPtr,
    is_running: bool,
}

unsafe impl Send for VirtualMachine {}
unsafe impl Sync for VirtualMachine {}

impl VirtualMachine {
    pub fn is_supported() -> bool {
        unsafe { ffi::vz_is_supported() != 0 }
    }

    /// Spawns the Linux Virtual Machine with connected IPC channel and console
    pub fn start(config: &VmConfig, console_fd: Option<i32>) -> Result<(Self, UnixStream)> {
        if !Self::is_supported() {
            return Err(MacletError::Vm(
                "Apple Virtualization Framework is not supported on this host".to_string(),
            ));
        }

        // Create socketpair for host-guest IPC
        let (host_sock, guest_sock) = StdUnixStream::pair().map_err(MacletError::Io)?;
        host_sock.set_nonblocking(true)?;
        let host_async = UnixStream::from_std(host_sock)?;
        let guest_fd = guest_sock.into_raw_fd();

        let kernel_str = config
            .kernel_path
            .to_str()
            .ok_or_else(|| MacletError::Vm("Invalid UTF-8 in kernel_path".to_string()))?;
        let kernel_c = CString::new(kernel_str)
            .map_err(|e| MacletError::Vm(format!("CString error: {e}")))?;

        let initrd_str = config
            .initramfs_path
            .to_str()
            .ok_or_else(|| MacletError::Vm("Invalid UTF-8 in initramfs_path".to_string()))?;
        let initrd_c = CString::new(initrd_str)
            .map_err(|e| MacletError::Vm(format!("CString error: {e}")))?;

        let cmdline_c = CString::new(config.cmdline.as_str())
            .map_err(|e| MacletError::Vm(format!("CString error: {e}")))?;

        let vz_cfg = VzConfig {
            cpu_count: config.cpu_count as u32,
            memory_bytes: config.memory_mb * 1024 * 1024,
            kernel_path: kernel_c.as_ptr(),
            initrd_path: initrd_c.as_ptr(),
            cmdline: cmdline_c.as_ptr(),
        };

        let mut vm_ptr: VzVmPtr = std::ptr::null_mut();
        let mut err_buf = [0i8; 1024];

        let ret = unsafe {
            ffi::vz_create_vm(
                &vz_cfg,
                console_fd.unwrap_or(-1),
                guest_fd,
                &mut vm_ptr,
                err_buf.as_mut_ptr(),
                err_buf.len(),
            )
        };

        if ret != 0 {
            let msg = unsafe { CStr::from_ptr(err_buf.as_ptr()).to_string_lossy().into_owned() };
            return Err(MacletError::Vm(format!("Failed to create VM: {msg}")));
        }

        let ret = unsafe { ffi::vz_start_vm(vm_ptr, err_buf.as_mut_ptr(), err_buf.len()) };
        if ret != 0 {
            let msg = unsafe { CStr::from_ptr(err_buf.as_ptr()).to_string_lossy().into_owned() };
            unsafe { ffi::vz_destroy_vm(vm_ptr) };
            return Err(MacletError::Vm(format!("Failed to start VM: {msg}")));
        }

        Ok((
            Self {
                vm_ptr,
                is_running: true,
            },
            host_async,
        ))
    }

    pub fn stop(&mut self) -> Result<()> {
        if !self.is_running {
            return Ok(());
        }

        let mut err_buf = [0i8; 1024];
        let ret = unsafe { ffi::vz_stop_vm(self.vm_ptr, err_buf.as_mut_ptr(), err_buf.len()) };
        self.is_running = false;

        if ret != 0 {
            let msg = unsafe { CStr::from_ptr(err_buf.as_ptr()).to_string_lossy().into_owned() };
            return Err(MacletError::Vm(format!("Failed to stop VM: {msg}")));
        }
        Ok(())
    }
}

impl Drop for VirtualMachine {
    fn drop(&mut self) {
        if self.is_running {
            let _ = self.stop();
        }
        if !self.vm_ptr.is_null() {
            unsafe { ffi::vz_destroy_vm(self.vm_ptr) };
            self.vm_ptr = std::ptr::null_mut();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vz_is_supported() {
        assert!(VirtualMachine::is_supported());
    }
}
