use flate2::read::GzDecoder;
use maclet_common::{
    ContainerInfo, ContainerInspectDetails, ContainerStatus, GuestMessage, HostMessage,
    ResourceLimits, Result,
};
use maclet_container::Container;
use maclet_ipc::{read_sync, write_sync};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread::sleep;
use std::time::{Duration, Instant};
use tar::Archive;

#[cfg(target_os = "linux")]
#[repr(C)]
struct Termios {
    c_iflag: u32,
    c_oflag: u32,
    c_cflag: u32,
    c_lflag: u32,
    c_line: u8,
    c_cc: [u8; 32],
    c_ispeed: u32,
    c_ospeed: u32,
}

#[cfg(target_os = "linux")]
extern "C" {
    fn tcgetattr(fd: i32, termios_p: *mut Termios) -> i32;
    fn tcsetattr(fd: i32, optional_actions: i32, termios_p: *const Termios) -> i32;
    fn cfmakeraw(termios_p: *mut Termios);
}

#[cfg(target_os = "linux")]
unsafe fn load_kernel_module(path: &Path) {
    use std::io::Read;
    if let Ok(mut f) = File::open(path) {
        let mut data = Vec::new();
        if f.read_to_end(&mut data).is_ok() {
            let empty = b"\0".as_ptr() as *const libc::c_char;
            let ret = libc::syscall(
                105, // __NR_init_module on arm64
                data.as_ptr() as *const libc::c_void,
                data.len() as libc::size_t,
                empty,
            );
            if ret != 0 {
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() != Some(17) {
                    eprintln!("[Kernel Module] Failed to load {}: {}", path.display(), err);
                }
            }
        }
    }
}

#[cfg(target_os = "linux")]
unsafe fn mount_pseudo_filesystems() {
    // Mount proc
    if !Path::new("/proc/uptime").exists() {
        let _ = fs::create_dir_all("/proc");
        libc_mount(
            b"proc\0".as_ptr(),
            b"/proc\0".as_ptr(),
            b"proc\0".as_ptr(),
            0,
            std::ptr::null(),
        );
    }
    // Mount sysfs
    if !Path::new("/sys/kernel").exists() {
        let _ = fs::create_dir_all("/sys");
        libc_mount(
            b"sysfs\0".as_ptr(),
            b"/sys\0".as_ptr(),
            b"sysfs\0".as_ptr(),
            0,
            std::ptr::null(),
        );
    }
    // Mount devtmpfs
    if !Path::new("/dev/null").exists() {
        let _ = fs::create_dir_all("/dev");
        libc_mount(
            b"devtmpfs\0".as_ptr(),
            b"/dev\0".as_ptr(),
            b"devtmpfs\0".as_ptr(),
            0,
            std::ptr::null(),
        );
    }
}

#[cfg(target_os = "linux")]
#[allow(dead_code)]
unsafe fn switch_to_tmpfs_root() {
    let mountinfo = fs::read_to_string("/proc/self/mountinfo").unwrap_or_default();
    if !mountinfo.contains("rootfs / rootfs") {
        return;
    }

    // 1. Create /sysroot
    let _ = fs::create_dir_all("/sysroot");
    let sysroot_c = b"/sysroot\0".as_ptr();
    let tmpfs_c = b"tmpfs\0".as_ptr();

    if libc::mount(tmpfs_c, sysroot_c, tmpfs_c, 0, std::ptr::null()) != 0 {
        eprintln!(
            "[Init] Failed to mount tmpfs on /sysroot: {}",
            std::io::Error::last_os_error()
        );
        return;
    }

    // 2. Unpack /rootfs.tar.gz into /sysroot if present
    if Path::new("/rootfs.tar.gz").exists() {
        if let Ok(f) = File::open("/rootfs.tar.gz") {
            let gz = GzDecoder::new(f);
            let mut archive = Archive::new(gz);
            let _ = archive.unpack("/sysroot");
        }
    }

    // Also populate base rootfs for containers
    let base_dir = "/sysroot/var/lib/maclet/rootfs";
    let _ = fs::create_dir_all(base_dir);
    if Path::new("/rootfs.tar.gz").exists() {
        if let Ok(f) = File::open("/rootfs.tar.gz") {
            let gz = GzDecoder::new(f);
            let mut archive = Archive::new(gz);
            let _ = archive.unpack(base_dir);
        }
    }

    let _ = fs::create_dir_all("/sysroot/proc");
    let _ = fs::create_dir_all("/sysroot/sys");
    let _ = fs::create_dir_all("/sysroot/dev");
    let _ = fs::create_dir_all("/sysroot/lib/modules");

    if let Ok(entries) = fs::read_dir("/lib/modules") {
        for entry in entries.flatten() {
            let dst = Path::new("/sysroot/lib/modules").join(entry.file_name());
            let _ = fs::copy(entry.path(), dst);
        }
    }

    // 3. Move pseudo filesystems to /sysroot
    let proc_c = b"/proc\0".as_ptr();
    let sys_c = b"/sys\0".as_ptr();
    let dev_c = b"/dev\0".as_ptr();

    let sysroot_proc = b"/sysroot/proc\0".as_ptr();
    let sysroot_sys = b"/sysroot/sys\0".as_ptr();
    let sysroot_dev = b"/sysroot/dev\0".as_ptr();

    libc::mount(
        proc_c,
        sysroot_proc,
        std::ptr::null(),
        libc::MS_MOVE,
        std::ptr::null(),
    );
    libc::mount(
        sys_c,
        sysroot_sys,
        std::ptr::null(),
        libc::MS_MOVE,
        std::ptr::null(),
    );
    libc::mount(
        dev_c,
        sysroot_dev,
        std::ptr::null(),
        libc::MS_MOVE,
        std::ptr::null(),
    );

    // 4. Move /sysroot over /
    let slash_c = b"/\0".as_ptr();
    let dot_c = b".\0".as_ptr();

    if libc::mount(
        sysroot_c,
        slash_c,
        std::ptr::null(),
        libc::MS_MOVE,
        std::ptr::null(),
    ) == 0
    {
        libc::chroot(dot_c);
        libc::chdir(slash_c);
    } else {
        eprintln!(
            "[Init] Failed to MS_MOVE /sysroot to /: {}",
            std::io::Error::last_os_error()
        );
    }
}

#[cfg(target_os = "linux")]
extern "C" {
    #[link_name = "mount"]
    fn libc_mount(
        source: *const u8,
        target: *const u8,
        fstype: *const u8,
        flags: u64,
        data: *const u8,
    ) -> i32;

    #[link_name = "mknod"]
    fn libc_mknod(pathname: *const u8, mode: u32, dev: u64) -> i32;

    #[link_name = "reboot"]
    fn libc_reboot(cmd: i32) -> i32;
}

#[allow(dead_code)]
const LINUX_REBOOT_CMD_POWER_OFF: i32 = 0x4321fedc;

fn ensure_device_nodes() {
    #[cfg(target_os = "linux")]
    unsafe {
        if !Path::new("/dev/hvc1").exists() {
            let _ = fs::create_dir_all("/dev");
            let dev_id = (229 << 8) | 1;
            libc_mknod(b"/dev/hvc1\0".as_ptr(), 0o20666, dev_id);
        }
    }
}

fn open_channel() -> std::io::Result<File> {
    #[cfg(target_os = "linux")]
    use std::os::unix::io::AsRawFd;

    ensure_device_nodes();
    let dev_path = "/dev/hvc1";
    let start = Instant::now();
    loop {
        match OpenOptions::new().read(true).write(true).open(dev_path) {
            Ok(file) => {
                #[cfg(target_os = "linux")]
                unsafe {
                    let mut term: Termios = std::mem::zeroed();
                    if tcgetattr(file.as_raw_fd(), &mut term) == 0 {
                        cfmakeraw(&mut term);
                        tcsetattr(file.as_raw_fd(), 0, &term);
                    }
                }
                return Ok(file);
            }
            Err(e) => {
                if start.elapsed() > Duration::from_secs(5) {
                    return Err(e);
                }
                sleep(Duration::from_millis(50));
            }
        }
    }
}

fn get_uptime_secs() -> u64 {
    fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|s| {
            s.split_whitespace()
                .next()
                .and_then(|val| val.parse::<f64>().ok())
        })
        .map(|v| v as u64)
        .unwrap_or(0)
}

fn ensure_base_rootfs() {
    let target_dir = Path::new("/var/lib/maclet/rootfs");
    let archive_path = Path::new("/rootfs.tar.gz");

    if archive_path.exists() {
        if !Path::new("/bin/sh").exists() {
            if let Ok(f) = File::open(archive_path) {
                let gz = GzDecoder::new(f);
                let mut archive = Archive::new(gz);
                let _ = archive.unpack("/");
            }
        }
        if !target_dir.join("bin/sh").exists() {
            let _ = fs::create_dir_all(target_dir);
            if let Ok(f) = File::open(archive_path) {
                let gz = GzDecoder::new(f);
                let mut archive = Archive::new(gz);
                let _ = archive.unpack(target_dir);
            }
        }
    }

    let _ = fs::create_dir_all("/etc");
    let _ = fs::write("/etc/resolv.conf", "nameserver 1.1.1.1\nnameserver 8.8.8.8\n");
    let _ = fs::create_dir_all("/var/lib/maclet/rootfs/etc");
    let _ = fs::write("/var/lib/maclet/rootfs/etc/resolv.conf", "nameserver 1.1.1.1\nnameserver 8.8.8.8\n");
}

#[cfg(target_os = "linux")]
#[repr(C)]
struct libc_utsname {
    sysname: [std::ffi::c_char; 65],
    nodename: [std::ffi::c_char; 65],
    release: [std::ffi::c_char; 65],
    version: [std::ffi::c_char; 65],
    machine: [std::ffi::c_char; 65],
    domainname: [std::ffi::c_char; 65],
}

#[cfg(target_os = "linux")]
extern "C" {
    #[link_name = "uname"]
    fn libc_uname(buf: *mut libc_utsname) -> i32;
}

fn handle_exec(
    id: String,
    cmd_name: String,
    args: Vec<String>,
    env: HashMap<String, String>,
) -> GuestMessage {
    // 1. Built-in command fallbacks
    match cmd_name.as_str() {
        "uname" => {
            #[cfg(target_os = "linux")]
            unsafe {
                let mut u: libc_utsname = std::mem::zeroed();
                if libc_uname(&mut u) == 0 {
                    let sysname =
                        std::ffi::CStr::from_ptr(u.sysname.as_ptr() as *const _).to_string_lossy();
                    let nodename =
                        std::ffi::CStr::from_ptr(u.nodename.as_ptr() as *const _).to_string_lossy();
                    let release =
                        std::ffi::CStr::from_ptr(u.release.as_ptr() as *const _).to_string_lossy();
                    let version =
                        std::ffi::CStr::from_ptr(u.version.as_ptr() as *const _).to_string_lossy();
                    let machine =
                        std::ffi::CStr::from_ptr(u.machine.as_ptr() as *const _).to_string_lossy();
                    let out =
                        if args.contains(&"-a".to_string()) || args.contains(&"-all".to_string()) {
                            format!(
                                "{} {} {} {} {}\n",
                                sysname, nodename, release, version, machine
                            )
                        } else if args.contains(&"-r".to_string()) {
                            format!("{}\n", release)
                        } else if args.contains(&"-m".to_string()) {
                            format!("{}\n", machine)
                        } else {
                            format!("{}\n", sysname)
                        };
                    return GuestMessage::ExecResult {
                        id,
                        exit_code: 0,
                        stdout: out,
                        stderr: String::new(),
                    };
                }
            }
        }
        "uptime" => {
            let uptime = get_uptime_secs();
            return GuestMessage::ExecResult {
                id,
                exit_code: 0,
                stdout: format!("up {} seconds\n", uptime),
                stderr: String::new(),
            };
        }
        "echo" => {
            return GuestMessage::ExecResult {
                id,
                exit_code: 0,
                stdout: format!("{}\n", args.join(" ")),
                stderr: String::new(),
            };
        }
        "cat" => {
            if let Some(path) = args.first() {
                match fs::read_to_string(path) {
                    Ok(content) => {
                        return GuestMessage::ExecResult {
                            id,
                            exit_code: 0,
                            stdout: content,
                            stderr: String::new(),
                        };
                    }
                    Err(e) => {
                        return GuestMessage::ExecResult {
                            id,
                            exit_code: 1,
                            stdout: String::new(),
                            stderr: format!("cat: {}: {}\n", path, e),
                        };
                    }
                }
            }
        }
        _ => {}
    }

    // 2. Spawn external process
    let mut cmd = Command::new(&cmd_name);
    cmd.args(&args);
    if !env.contains_key("PATH") {
        cmd.env("PATH", "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin");
    }
    for (k, v) in env {
        cmd.env(k, v);
    }

    match cmd.output() {
        Ok(output) => GuestMessage::ExecResult {
            id,
            exit_code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        },
        Err(e) => GuestMessage::ExecResult {
            id,
            exit_code: 127,
            stdout: String::new(),
            stderr: format!("Failed to spawn command '{}': {}\n", cmd_name, e),
        },
    }
}

fn current_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_else(|_| get_uptime_secs())
}

fn is_pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(target_os = "linux")]
    unsafe {
        libc::kill(pid as i32, 0) == 0
    }
    #[cfg(not(target_os = "linux"))]
    false
}

fn handle_run_container(
    id: String,
    name: Option<String>,
    image: String,
    command: String,
    args: Vec<String>,
    env: HashMap<String, String>,
    hostname: Option<String>,
    limits: Option<ResourceLimits>,
    detached: bool,
    ports: Vec<String>,
    read_only: bool,
    drop_caps: Vec<String>,
) -> GuestMessage {
    let base_rootfs = Path::new("/var/lib/maclet/rootfs");
    let container_name = name.unwrap_or_else(|| {
        let short = if id.len() > 12 { &id[..12] } else { &id };
        format!("cnt-{}", short)
    });

    let assigned_ip = if !ports.is_empty() {
        let net_mgr = maclet_network::NetworkManager::new();
        Some(net_mgr.allocate_ip())
    } else {
        None
    };

    match Container::new(
        id.clone(),
        base_rootfs,
        command.clone(),
        args.clone(),
        env.clone(),
        hostname.clone(),
        limits.clone(),
        ports.clone(),
        read_only,
        drop_caps.clone(),
        assigned_ip,
    ) {
        Ok(container) => {
            let container_dir = PathBuf::from(format!("/var/lib/maclet/containers/{}", id));
            let _ = fs::create_dir_all(&container_dir);
            let _ = fs::create_dir_all("/var/log/maclet");

            let log_path = PathBuf::from(format!("/var/log/maclet/{}.log", id));
            let exit_file = container_dir.join("exit_code");

            let now = current_timestamp();
            let mut details = ContainerInspectDetails {
                id: id.clone(),
                name: container_name.clone(),
                image: if image.is_empty() {
                    "alpine:latest".to_string()
                } else {
                    image
                },
                command: command.clone(),
                args: args.clone(),
                env: env.clone(),
                hostname: container.hostname.clone(),
                status: ContainerStatus::Created,
                created_at: now,
                started_at: None,
                finished_at: None,
                pid: None,
                exit_code: None,
                limits: limits.clone(),
                metrics: None,
                rootfs_path: container.rootfs.to_string_lossy().to_string(),
                log_path: log_path.to_string_lossy().to_string(),
                ip: assigned_ip.map(|ip| ip.to_string()),
                ports: ports.clone(),
                read_only,
                drop_caps,
            };

            if detached {
                match container.spawn_detached(&log_path, &exit_file) {
                    Ok((sup_pid, c_pid)) => {
                        let _ = fs::write(container_dir.join("supervisor.pid"), format!("{}\n", sup_pid));
                        let _ = fs::write(container_dir.join("pid"), format!("{}\n", c_pid));

                        details.status = ContainerStatus::Running;
                        details.started_at = Some(now);
                        details.pid = Some(c_pid);

                        let meta_path = container_dir.join("meta.json");
                        let _ = fs::write(
                            meta_path,
                            serde_json::to_string_pretty(&details).unwrap_or_default(),
                        );

                        GuestMessage::ContainerStarted {
                            id,
                            pid: c_pid,
                            name: container_name,
                        }
                    }
                    Err(e) => {
                        container.cleanup();
                        GuestMessage::Error {
                            id: Some(id),
                            message: format!("Failed to spawn detached container: {}", e),
                        }
                    }
                }
            } else {
                details.status = ContainerStatus::Running;
                details.started_at = Some(now);

                let res = match container.run() {
                    Ok((exec_res, metrics)) => {
                        details.status = ContainerStatus::Exited;
                        details.finished_at = Some(current_timestamp());
                        details.exit_code = Some(exec_res.exit_code);
                        details.metrics = metrics.clone();

                        GuestMessage::ContainerResult {
                            id: id.clone(),
                            exit_code: exec_res.exit_code,
                            stdout: exec_res.stdout,
                            stderr: exec_res.stderr,
                            metrics,
                        }
                    }
                    Err(e) => GuestMessage::ContainerResult {
                        id: id.clone(),
                        exit_code: 126,
                        stdout: String::new(),
                        stderr: format!("Failed to run container: {}\n", e),
                        metrics: None,
                    },
                };
                container.cleanup();
                res
            }
        }
        Err(e) => GuestMessage::ContainerResult {
            id,
            exit_code: 125,
            stdout: String::new(),
            stderr: format!("Failed to initialize container: {}\n", e),
            metrics: None,
        },
    }
}

fn handle_list_containers(
    all: bool,
    cgroup_mgr: &maclet_cgroups::CgroupV2Manager,
) -> GuestMessage {
    let containers_dir = Path::new("/var/lib/maclet/containers");
    let mut list = Vec::new();

    if let Ok(entries) = fs::read_dir(containers_dir) {
        for entry in entries.flatten() {
            let meta_path = entry.path().join("meta.json");
            if let Ok(meta_str) = fs::read_to_string(&meta_path) {
                if let Ok(mut details) = serde_json::from_str::<ContainerInspectDetails>(&meta_str) {
                    let pid_file = entry.path().join("pid");
                    let pid = fs::read_to_string(pid_file)
                        .ok()
                        .and_then(|s| s.trim().parse::<u32>().ok())
                        .or(details.pid);

                    if let Some(p) = pid {
                        if is_pid_alive(p) {
                            details.status = ContainerStatus::Running;
                        } else {
                            let exit_file = entry.path().join("exit_code");
                            if let Ok(code_str) = fs::read_to_string(exit_file) {
                                if let Ok(code) = code_str.trim().parse::<i32>() {
                                    details.exit_code = Some(code);
                                }
                            }
                            if details.status == ContainerStatus::Running {
                                details.status = ContainerStatus::Exited;
                            }
                        }
                    }

                    if !all && details.status != ContainerStatus::Running {
                        continue;
                    }

                    let metrics = cgroup_mgr.read_metrics(&details.id).ok();
                    let mem_cur = metrics.as_ref().map(|m| m.memory_current);
                    let mem_max = metrics.as_ref().and_then(|m| m.memory_max);

                    let cmd_display = if details.args.is_empty() {
                        details.command.clone()
                    } else {
                        format!("{} {}", details.command, details.args.join(" "))
                    };

                    list.push(ContainerInfo {
                        id: details.id,
                        name: details.name,
                        image: details.image,
                        command: cmd_display,
                        status: details.status,
                        created_at: details.created_at,
                        pid,
                        exit_code: details.exit_code,
                        memory_current: mem_cur,
                        memory_max: mem_max,
                        ip: details.ip,
                        ports: details.ports,
                    });
                }
            }
        }
    }

    list.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    GuestMessage::ContainerList { containers: list }
}

fn find_container(target: &str) -> Option<(String, PathBuf)> {
    let exact = PathBuf::from(format!("/var/lib/maclet/containers/{}", target));
    if exact.exists() {
        return Some((target.to_string(), exact));
    }

    if let Ok(entries) = fs::read_dir("/var/lib/maclet/containers") {
        for entry in entries.flatten() {
            let p = entry.path();
            let dirname = entry.file_name().to_string_lossy().to_string();
            if dirname.starts_with(target) {
                return Some((dirname, p));
            }
            let meta_file = p.join("meta.json");
            if let Ok(meta_str) = fs::read_to_string(meta_file) {
                if let Ok(details) = serde_json::from_str::<ContainerInspectDetails>(&meta_str) {
                    if details.name == target {
                        return Some((details.id, p));
                    }
                }
            }
        }
    }
    None
}

fn handle_stop_container(target: String, timeout_secs: u64) -> GuestMessage {
    let (id, container_dir) = match find_container(&target) {
        Some(pair) => pair,
        None => {
            return GuestMessage::Error {
                id: Some(target),
                message: "No such container".to_string(),
            };
        }
    };

    let pid_file = container_dir.join("pid");
    let pid = fs::read_to_string(&pid_file)
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok());

    let mut exit_code = None;

    if let Some(p) = pid {
        if is_pid_alive(p) {
            #[cfg(target_os = "linux")]
            unsafe {
                libc::kill(p as i32, libc::SIGTERM);
            }

            // Wait up to timeout_secs
            let iters = timeout_secs * 10;
            let mut stopped = false;
            for _ in 0..iters {
                sleep(Duration::from_millis(100));
                if !is_pid_alive(p) {
                    stopped = true;
                    break;
                }
            }

            if !stopped {
                #[cfg(target_os = "linux")]
                unsafe {
                    libc::kill(p as i32, libc::SIGKILL);
                }
                sleep(Duration::from_millis(100));
            }
        }
    }

    let exit_file = container_dir.join("exit_code");
    if let Ok(s) = fs::read_to_string(&exit_file) {
        if let Ok(c) = s.trim().parse::<i32>() {
            exit_code = Some(c);
        }
    }

    // Update meta.json
    let meta_path = container_dir.join("meta.json");
    if let Ok(meta_str) = fs::read_to_string(&meta_path) {
        if let Ok(mut details) = serde_json::from_str::<ContainerInspectDetails>(&meta_str) {
            details.status = ContainerStatus::Stopped;
            details.finished_at = Some(current_timestamp());
            details.exit_code = exit_code;
            let _ = fs::write(meta_path, serde_json::to_string_pretty(&details).unwrap_or_default());
        }
    }

    GuestMessage::ContainerStopped { id, exit_code }
}

fn handle_remove_container(
    target: String,
    force: bool,
    cgroup_mgr: &maclet_cgroups::CgroupV2Manager,
) -> GuestMessage {
    let (id, container_dir) = match find_container(&target) {
        Some(pair) => pair,
        None => {
            return GuestMessage::Error {
                id: Some(target.clone()),
                message: format!("No such container: {}", target),
            };
        }
    };

    let pid_file = container_dir.join("pid");
    let pid = fs::read_to_string(&pid_file)
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok());

    if let Some(p) = pid {
        if is_pid_alive(p) {
            if !force {
                return GuestMessage::Error {
                    id: Some(id),
                    message: "Container is running. Stop container first or use --force.".to_string(),
                };
            }
            #[cfg(target_os = "linux")]
            unsafe {
                libc::kill(p as i32, libc::SIGKILL);
            }
            sleep(Duration::from_millis(100));
        }
    }

    // Unmount rootfs
    #[cfg(target_os = "linux")]
    {
        let rootfs_dir = container_dir.join("rootfs");
        if let Ok(dst_c) = std::ffi::CString::new(rootfs_dir.to_str().unwrap()) {
            unsafe {
                libc::umount2(dst_c.as_ptr(), libc::MNT_DETACH);
            }
        }
    }

    // Clean up cgroup
    cgroup_mgr.destroy_container_cgroup(&id);

    // Remove directories and logs
    let _ = fs::remove_dir_all(&container_dir);
    let log_path = format!("/var/log/maclet/{}.log", id);
    let _ = fs::remove_file(log_path);

    GuestMessage::ContainerRemoved { id }
}

fn handle_container_logs(target: String, tail: Option<usize>) -> GuestMessage {
    let (id, _) = match find_container(&target) {
        Some(pair) => pair,
        None => {
            return GuestMessage::Error {
                id: Some(target.clone()),
                message: format!("No such container: {}", target),
            };
        }
    };
    let log_path = format!("/var/log/maclet/{}.log", id);
    match fs::read_to_string(&log_path) {
        Ok(content) => {
            let logs = if let Some(n) = tail {
                let lines: Vec<&str> = content.lines().collect();
                if lines.len() > n {
                    lines[lines.len() - n..].join("\n") + "\n"
                } else {
                    content
                }
            } else {
                content
            };
            GuestMessage::ContainerLogsResult { id, logs }
        }
        Err(_) => GuestMessage::ContainerLogsResult {
            id,
            logs: String::new(),
        },
    }
}

fn handle_exec_in_container(
    target: String,
    command: String,
    args: Vec<String>,
    env: HashMap<String, String>,
) -> GuestMessage {
    let (id, container_dir) = match find_container(&target) {
        Some(pair) => pair,
        None => {
            return GuestMessage::Error {
                id: Some(target.clone()),
                message: format!("No such container: {}", target),
            };
        }
    };

    let pid_file = container_dir.join("pid");
    let pid = match fs::read_to_string(&pid_file)
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
    {
        Some(p) => p,
        None => {
            return GuestMessage::Error {
                id: Some(id),
                message: "Container PID not found".to_string(),
            };
        }
    };

    if !is_pid_alive(pid) {
        return GuestMessage::Error {
            id: Some(id),
            message: "Container is not running".to_string(),
        };
    }

    match maclet_namespaces::exec_in_existing_container(pid, &command, &args, &env) {
        Ok(res) => GuestMessage::ContainerExecResult {
            id,
            exit_code: res.exit_code,
            stdout: res.stdout,
            stderr: res.stderr,
        },
        Err(e) => GuestMessage::Error {
            id: Some(id),
            message: format!("Exec failed: {}", e),
        },
    }
}

fn handle_inspect_container(
    target: String,
    cgroup_mgr: &maclet_cgroups::CgroupV2Manager,
) -> GuestMessage {
    let (id, container_dir) = match find_container(&target) {
        Some(pair) => pair,
        None => {
            return GuestMessage::Error {
                id: Some(target.clone()),
                message: format!("Container '{}' not found", target),
            };
        }
    };
    let meta_path = container_dir.join("meta.json");

    if let Ok(meta_str) = fs::read_to_string(&meta_path) {
        if let Ok(mut details) = serde_json::from_str::<ContainerInspectDetails>(&meta_str) {
            let pid_file = container_dir.join("pid");
            let pid = fs::read_to_string(pid_file)
                .ok()
                .and_then(|s| s.trim().parse::<u32>().ok())
                .or(details.pid);

            if let Some(p) = pid {
                if is_pid_alive(p) {
                    details.status = ContainerStatus::Running;
                } else {
                    let exit_file = container_dir.join("exit_code");
                    if let Ok(code_str) = fs::read_to_string(exit_file) {
                        if let Ok(code) = code_str.trim().parse::<i32>() {
                            details.exit_code = Some(code);
                        }
                    }
                    if details.status == ContainerStatus::Running {
                        details.status = ContainerStatus::Exited;
                    }
                }
            }

            details.metrics = cgroup_mgr.read_metrics(&id).ok();

            return GuestMessage::InspectResult {
                id,
                details: Box::new(details),
            };
        }
    }

    GuestMessage::Error {
        id: Some(id.clone()),
        message: format!("Container '{}' not found", id),
    }
}

fn run_agent() -> Result<()> {
    std::env::set_var(
        "PATH",
        "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
    );

    #[cfg(target_os = "linux")]
    if std::process::id() == 1 {
        unsafe {
            mount_pseudo_filesystems();
        };
    }

    ensure_base_rootfs();

    // Initialize cgroups v2 hierarchy
    let cgroup_mgr = maclet_cgroups::CgroupV2Manager::new();
    let _ = cgroup_mgr.init();

    #[cfg(target_os = "linux")]
    {
        // Load core kernel modules
        unsafe {
            load_kernel_module(Path::new("/lib/modules/overlay.ko"));
            load_kernel_module(Path::new("/lib/modules/squashfs.ko"));
            load_kernel_module(Path::new("/lib/modules/loop.ko"));
            load_kernel_module(Path::new("/lib/modules/virtio_blk.ko"));
        }

        // Mount /dev/vda if attached
        if Path::new("/dev/vda").exists() {
            let _ = fs::create_dir_all("/.modloop");
            let vda_c = std::ffi::CString::new("/dev/vda").unwrap();
            let mnt_c = std::ffi::CString::new("/.modloop").unwrap();
            let sq_c = std::ffi::CString::new("squashfs").unwrap();
            unsafe {
                libc::mount(
                    vda_c.as_ptr(),
                    mnt_c.as_ptr(),
                    sq_c.as_ptr(),
                    libc::MS_RDONLY,
                    std::ptr::null(),
                );
            }
            if let Ok(entries) = fs::read_dir("/.modloop/modules") {
                let _ = fs::create_dir_all("/lib/modules");
                for entry in entries.flatten() {
                    let kver = entry.file_name();
                    let target = Path::new("/lib/modules").join(&kver);
                    let _ = std::os::unix::fs::symlink(entry.path(), target);
                }
            }
            let _ = Command::new("modprobe").arg("bridge").status();
            let _ = Command::new("modprobe").arg("veth").status();
        }

        // Load network drivers
        unsafe {
            load_kernel_module(Path::new("/lib/modules/af_packet.ko"));
            load_kernel_module(Path::new("/lib/modules/failover.ko"));
            load_kernel_module(Path::new("/lib/modules/net_failover.ko"));
            load_kernel_module(Path::new("/lib/modules/virtio_net.ko"));
        }

        // Initialize guest eth0 NAT interface if present
        if Path::new("/sys/class/net/eth0").exists() {
            let _ = Command::new("ip").args(["link", "set", "eth0", "up"]).status();
            let _ = Command::new("udhcpc")
                .args(["-i", "eth0", "-q", "-n", "-t", "3"])
                .status();
        }

        // Initialize host bridge maclet0
        let net_mgr = maclet_network::NetworkManager::new();
        let _ = net_mgr.init_host_network();
    }

    let _ = fs::create_dir_all("/var/lib/maclet/containers");
    let _ = fs::create_dir_all("/var/log/maclet");

    let file = open_channel()?;
    let mut reader = BufReader::new(file.try_clone()?);
    let mut writer = BufWriter::new(file);

    // Send Ready event
    let ready = GuestMessage::Ready {
        version: env!("CARGO_PKG_VERSION").to_string(),
        arch: std::env::consts::ARCH.to_string(),
        uptime_secs: get_uptime_secs(),
    };
    write_sync(&mut writer, &ready)?;

    // Process incoming host requests
    while let Some(msg) = read_sync::<_, HostMessage>(&mut reader)? {
        match msg {
            HostMessage::Ping { id } => {
                write_sync(&mut writer, &GuestMessage::Pong { id })?;
            }
            HostMessage::Exec {
                id,
                command,
                args,
                env,
                timeout_secs: _,
            } => {
                let result = handle_exec(id, command, args, env);
                write_sync(&mut writer, &result)?;
            }
            HostMessage::RunContainer {
                id,
                name,
                image,
                command,
                args,
                env,
                hostname,
                limits,
                detached,
                ports,
                read_only,
                drop_caps,
                timeout_secs: _,
            } => {
                let result = handle_run_container(
                    id,
                    name,
                    image,
                    command,
                    args,
                    env,
                    hostname,
                    limits,
                    detached,
                    ports,
                    read_only,
                    drop_caps,
                );
                write_sync(&mut writer, &result)?;
            }
            HostMessage::ListContainers { all } => {
                let result = handle_list_containers(all, &cgroup_mgr);
                write_sync(&mut writer, &result)?;
            }
            HostMessage::StopContainer { id, timeout_secs } => {
                let result = handle_stop_container(id, timeout_secs);
                write_sync(&mut writer, &result)?;
            }
            HostMessage::RemoveContainer { id, force } => {
                let result = handle_remove_container(id, force, &cgroup_mgr);
                write_sync(&mut writer, &result)?;
            }
            HostMessage::ContainerLogs { id, tail } => {
                let result = handle_container_logs(id, tail);
                write_sync(&mut writer, &result)?;
            }
            HostMessage::ExecInContainer {
                id,
                command,
                args,
                env,
            } => {
                let result = handle_exec_in_container(id, command, args, env);
                write_sync(&mut writer, &result)?;
            }
            HostMessage::InspectContainer { id } => {
                let result = handle_inspect_container(id, &cgroup_mgr);
                write_sync(&mut writer, &result)?;
            }
            HostMessage::Shutdown { id } => {
                let _ = write_sync(&mut writer, &GuestMessage::Pong { id });
                #[cfg(target_os = "linux")]
                unsafe {
                    libc_reboot(LINUX_REBOOT_CMD_POWER_OFF);
                }
                break;
            }
        }
    }

    Ok(())
}

fn main() {
    if let Err(e) = run_agent() {
        eprintln!("[maclet-guest-agent] Fatal error: {}", e);
    }

    if std::process::id() == 1 {
        loop {
            sleep(Duration::from_secs(60));
        }
    }
}
