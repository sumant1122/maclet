use maclet_common::{GuestMessage, HostMessage, Result};
use maclet_ipc::{read_sync, write_sync};
use std::fs::{File, OpenOptions};
use std::io::{BufReader, BufWriter};
#[cfg(target_os = "linux")]
use std::path::Path;
use std::process::Command;
use std::thread::sleep;
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
unsafe fn mount_pseudo_filesystems() {
    // Mount proc
    if !Path::new("/proc/uptime").exists() {
        let _ = std::fs::create_dir_all("/proc");
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
        let _ = std::fs::create_dir_all("/sys");
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
        let _ = std::fs::create_dir_all("/dev");
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
            let _ = std::fs::create_dir_all("/dev");
            // hvc major is 229, hvc1 is minor 1
            let dev_id = (229 << 8) | 1;
            libc_mknod(b"/dev/hvc1\0".as_ptr(), 0o20666, dev_id);
        }
    }
}

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
    std::fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|s| {
            s.split_whitespace()
                .next()
                .and_then(|val| val.parse::<f64>().ok())
        })
        .map(|v| v as u64)
        .unwrap_or(0)
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
    env: std::collections::HashMap<String, String>,
) -> GuestMessage {
    // 1. Built-in command fallbacks
    match cmd_name.as_str() {
        "uname" => {
            #[cfg(target_os = "linux")]
            unsafe {
                let mut u: libc_utsname = std::mem::zeroed();
                if libc_uname(&mut u) == 0 {
                    let sysname = std::ffi::CStr::from_ptr(u.sysname.as_ptr() as *const _).to_string_lossy();
                    let nodename = std::ffi::CStr::from_ptr(u.nodename.as_ptr() as *const _).to_string_lossy();
                    let release = std::ffi::CStr::from_ptr(u.release.as_ptr() as *const _).to_string_lossy();
                    let version = std::ffi::CStr::from_ptr(u.version.as_ptr() as *const _).to_string_lossy();
                    let machine = std::ffi::CStr::from_ptr(u.machine.as_ptr() as *const _).to_string_lossy();
                    let out = if args.contains(&"-a".to_string()) || args.contains(&"-all".to_string()) {
                        format!("{} {} {} {} {}\n", sysname, nodename, release, version, machine)
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
                match std::fs::read_to_string(path) {
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

fn run_agent() -> Result<()> {
    #[cfg(target_os = "linux")]
    if std::process::id() == 1 {
        unsafe { mount_pseudo_filesystems() };
    }

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

    // If running as PID 1, never exit (which causes kernel panic)
    if std::process::id() == 1 {
        loop {
            sleep(Duration::from_secs(60));
        }
    }
}
