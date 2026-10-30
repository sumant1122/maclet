use flate2::read::GzDecoder;
use maclet_common::{GuestMessage, HostMessage, ResourceLimits, Result};
use maclet_container::Container;
use maclet_ipc::{read_sync, write_sync};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter};
use std::path::Path;
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
    if target_dir.join("bin/sh").exists() {
        return;
    }

    let archive_path = Path::new("/rootfs.tar.gz");
    if !archive_path.exists() {
        return;
    }

    let _ = fs::create_dir_all(target_dir);
    if let Ok(f) = File::open(archive_path) {
        let gz = GzDecoder::new(f);
        let mut archive = Archive::new(gz);
        let _ = archive.unpack(target_dir);
    }
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

fn handle_run_container(
    id: String,
    command: String,
    args: Vec<String>,
    env: HashMap<String, String>,
    hostname: Option<String>,
    limits: Option<ResourceLimits>,
) -> GuestMessage {
    let base_rootfs = Path::new("/var/lib/maclet/rootfs");
    match Container::new(id.clone(), base_rootfs, command, args, env, hostname, limits) {
        Ok(container) => {
            let res = match container.run() {
                Ok((exec_res, metrics)) => GuestMessage::ContainerResult {
                    id: id.clone(),
                    exit_code: exec_res.exit_code,
                    stdout: exec_res.stdout,
                    stderr: exec_res.stderr,
                    metrics,
                },
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
        Err(e) => GuestMessage::ContainerResult {
            id,
            exit_code: 125,
            stdout: String::new(),
            stderr: format!("Failed to initialize container: {}\n", e),
            metrics: None,
        },
    }
}

fn run_agent() -> Result<()> {
    #[cfg(target_os = "linux")]
    if std::process::id() == 1 {
        unsafe {
            mount_pseudo_filesystems();
            switch_to_tmpfs_root();
        };
    }

    // Initialize cgroups v2 hierarchy
    let cgroup_mgr = maclet_cgroups::CgroupV2Manager::new();
    let _ = cgroup_mgr.init();

    ensure_base_rootfs();

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
                image: _,
                command,
                args,
                env,
                hostname,
                limits,
                timeout_secs: _,
            } => {
                let result = handle_run_container(id, command, args, env, hostname, limits);
                write_sync(&mut writer, &result)?;
            }
            HostMessage::InspectContainer { id } => {
                let metrics = cgroup_mgr.read_metrics(&id).unwrap_or_default();
                write_sync(
                    &mut writer,
                    &GuestMessage::InspectResult {
                        id,
                        state: "active".to_string(),
                        metrics,
                    },
                )?;
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
