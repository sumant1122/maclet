use std::os::raw::c_char;

pub enum VzVmOpaque {}
pub type VzVmPtr = *mut VzVmOpaque;

#[repr(C)]
pub struct VzConfig {
    pub cpu_count: u32,
    pub memory_bytes: u64,
    pub kernel_path: *const c_char,
    pub initrd_path: *const c_char,
    pub cmdline: *const c_char,
}

extern "C" {
    pub fn vz_is_supported() -> i32;

    pub fn vz_create_vm(
        config: *const VzConfig,
        console_write_fd: i32,
        ipc_socket_fd: i32,
        out_vm: *mut VzVmPtr,
        err_buf: *mut c_char,
        err_len: usize,
    ) -> i32;

    pub fn vz_start_vm(
        vm: VzVmPtr,
        err_buf: *mut c_char,
        err_len: usize,
    ) -> i32;

    pub fn vz_stop_vm(
        vm: VzVmPtr,
        err_buf: *mut c_char,
        err_len: usize,
    ) -> i32;

    pub fn vz_destroy_vm(vm: VzVmPtr);
}
