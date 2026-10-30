use maclet_common::{MacletError, Result};
use std::collections::HashMap;
#[cfg(target_os = "linux")]
use std::ffi::CString;
#[cfg(target_os = "linux")]
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct ContainerConfig {
    pub id: String,
    pub hostname: String,
    pub rootfs: PathBuf,
    pub command: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub cgroup_path: Option<PathBuf>,
    pub netns_enabled: bool,
    pub assigned_ip: Option<std::net::Ipv4Addr>,
    pub ports: Vec<String>,
    pub read_only: bool,
    pub drop_caps: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ExecutionResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

#[cfg(target_os = "linux")]
const SYS_PIVOT_ROOT: i64 = 41; // __NR_pivot_root on arm64

#[cfg(target_os = "linux")]
unsafe fn pivot_root_syscall(new_root: *const libc::c_char, put_old: *const libc::c_char) -> i32 {
    libc::syscall(SYS_PIVOT_ROOT, new_root, put_old) as i32
}

/// Spawns an isolated container in background (detached mode).
/// Returns (supervisor_pid, container_init_pid).
pub fn spawn_isolated_namespaces(
    config: &ContainerConfig,
    log_path: &Path,
    exit_file: &Path,
) -> Result<(u32, u32)> {
    #[cfg(target_os = "linux")]
    return spawn_linux_namespaces(config, log_path, exit_file);

    #[cfg(not(target_os = "linux"))]
    {
        let _ = (config, log_path, exit_file);
        Err(MacletError::Guest(
            "Linux namespaces are only supported on Linux target".to_string(),
        ))
    }
}

/// Executes a command inside an already running container by joining its namespaces via setns().
pub fn exec_in_existing_container(
    container_pid: u32,
    command: &str,
    args: &[String],
    env: &HashMap<String, String>,
) -> Result<ExecutionResult> {
    #[cfg(target_os = "linux")]
    return exec_linux_namespaces(container_pid, command, args, env);

    #[cfg(not(target_os = "linux"))]
    {
        let _ = (container_pid, command, args, env);
        Err(MacletError::Guest(
            "Linux namespaces are only supported on Linux target".to_string(),
        ))
    }
}

#[cfg(target_os = "linux")]
fn spawn_linux_namespaces(
    config: &ContainerConfig,
    log_path: &Path,
    exit_file: &Path,
) -> Result<(u32, u32)> {
    let mut pid_pipe = [0i32; 2];
    unsafe {
        if libc::pipe(pid_pipe.as_mut_ptr()) != 0 {
            return Err(MacletError::Io(std::io::Error::last_os_error()));
        }
    }

    let intermediate_pid = unsafe { libc::fork() };
    if intermediate_pid < 0 {
        return Err(MacletError::Io(std::io::Error::last_os_error()));
    }

    if intermediate_pid == 0 {
        // --- Intermediate Supervisor Process ---
        unsafe {
            libc::close(pid_pipe[0]);
            // Create a new session so supervisor is detached
            libc::setsid();
        }

        let clone_flags = libc::CLONE_NEWNS
            | libc::CLONE_NEWUTS
            | libc::CLONE_NEWIPC
            | libc::CLONE_NEWPID;

        let ret = unsafe { libc::unshare(clone_flags) };
        if ret != 0 {
            unsafe {
                libc::close(pid_pipe[1]);
                libc::_exit(1);
            }
        }

        let mut sync_pipe = [0i32; 2];
        if config.netns_enabled {
            unsafe {
                if libc::pipe(sync_pipe.as_mut_ptr()) != 0 {
                    libc::close(pid_pipe[1]);
                    libc::_exit(1);
                }
            }
        }

        let container_pid = unsafe { libc::fork() };
        if container_pid < 0 {
            unsafe {
                libc::close(pid_pipe[1]);
                libc::_exit(1);
            }
        }

        if container_pid == 0 {
            // --- Container Init Process (PID 1 inside container PID namespace) ---
            unsafe {
                libc::close(pid_pipe[1]);
            }

            if config.netns_enabled {
                unsafe {
                    let _ = libc::unshare(libc::CLONE_NEWNET);
                    libc::close(sync_pipe[1]);
                    let mut b = 0u8;
                    libc::read(sync_pipe[0], &mut b as *mut u8 as *mut libc::c_void, 1);
                    libc::close(sync_pipe[0]);
                }
            }

            if let Some(ref cg) = config.cgroup_path {
                let procs_file = cg.join("cgroup.procs");
                let _ = fs::write(procs_file, "0\n");
                unsafe {
                    let _ = libc::unshare(libc::CLONE_NEWCGROUP);
                }
            }

            // Redirect stdout and stderr to the log file
            let log_path_c = CString::new(log_path.to_str().unwrap()).unwrap();
            let log_fd = unsafe {
                libc::open(
                    log_path_c.as_ptr(),
                    libc::O_CREAT | libc::O_WRONLY | libc::O_APPEND,
                    0o644,
                )
            };
            if log_fd >= 0 {
                unsafe {
                    libc::dup2(log_fd, libc::STDOUT_FILENO);
                    libc::dup2(log_fd, libc::STDERR_FILENO);
                    libc::close(log_fd);
                }
            }

            // Redirect stdin from /dev/null
            let null_c = CString::new("/dev/null").unwrap();
            let null_fd = unsafe { libc::open(null_c.as_ptr(), libc::O_RDONLY) };
            if null_fd >= 0 {
                unsafe {
                    libc::dup2(null_fd, libc::STDIN_FILENO);
                    libc::close(null_fd);
                }
            }

            if let Err(e) = setup_container_root(config) {
                eprintln!("[Container Init Error] {}", e);
                unsafe { libc::_exit(126) };
            }

            execute_payload(config);
            unsafe { libc::_exit(127) };
        }

        // Supervisor: send container_pid to parent
        unsafe {
            let pid_i32 = container_pid as i32;
            libc::write(
                pid_pipe[1],
                &pid_i32 as *const i32 as *const libc::c_void,
                std::mem::size_of::<i32>(),
            );
            libc::close(pid_pipe[1]);
        }

        // Supervisor: configure container network namespace if enabled
        if config.netns_enabled {
            unsafe {
                libc::close(sync_pipe[0]);
            }
            if let Some(ip) = config.assigned_ip {
                let net_mgr = maclet_network::NetworkManager::new();
                let _ = net_mgr.init_host_network();
                let _ = net_mgr.setup_container_network(&config.id, container_pid as u32, ip);
                for p in &config.ports {
                    if let Some((hp, cp)) = p.split_once(':') {
                        if let (Ok(h_port), Ok(c_port)) = (hp.parse::<u16>(), cp.parse::<u16>()) {
                            let _ = net_mgr.forward_port(h_port, ip, c_port);
                        }
                    }
                }
            }
            unsafe {
                let b = 1u8;
                libc::write(sync_pipe[1], &b as *const u8 as *const libc::c_void, 1);
                libc::close(sync_pipe[1]);
            }
        }

        // Wait for container init process to finish
        let mut status: i32 = 0;
        unsafe {
            libc::waitpid(container_pid, &mut status, 0);
        }

        if config.netns_enabled {
            let net_mgr = maclet_network::NetworkManager::new();
            let _ = net_mgr.teardown_container_network(&config.id);
        }
        let code = if libc::WIFEXITED(status) {
            libc::WEXITSTATUS(status)
        } else if libc::WIFSIGNALED(status) {
            128 + libc::WTERMSIG(status)
        } else {
            1
        };

        let _ = fs::write(exit_file, format!("{}\n", code));
        unsafe { libc::_exit(0) };
    }

    // --- Parent (Guest Agent) ---
    unsafe {
        libc::close(pid_pipe[1]);
    }
    let mut c_pid: i32 = 0;
    let n = unsafe {
        libc::read(
            pid_pipe[0],
            &mut c_pid as *mut i32 as *mut libc::c_void,
            std::mem::size_of::<i32>(),
        )
    };
    unsafe {
        libc::close(pid_pipe[0]);
    }

    if n <= 0 || c_pid <= 0 {
        return Err(MacletError::Guest(
            "Failed to receive container PID from intermediate supervisor".to_string(),
        ));
    }

    Ok((intermediate_pid as u32, c_pid as u32))
}

#[cfg(target_os = "linux")]
fn exec_linux_namespaces(
    container_pid: u32,
    command: &str,
    args: &[String],
    env: &HashMap<String, String>,
) -> Result<ExecutionResult> {
    use std::io::Read;
    use std::os::unix::io::FromRawFd;

    let mut stdout_pipe = [0i32; 2];
    let mut stderr_pipe = [0i32; 2];
    unsafe {
        if libc::pipe(stdout_pipe.as_mut_ptr()) != 0 || libc::pipe(stderr_pipe.as_mut_ptr()) != 0 {
            return Err(MacletError::Io(std::io::Error::last_os_error()));
        }
    }

    let helper_pid = unsafe { libc::fork() };
    if helper_pid < 0 {
        return Err(MacletError::Io(std::io::Error::last_os_error()));
    }

    if helper_pid == 0 {
        // --- Helper Process: Join container namespaces ---
        unsafe {
            libc::close(stdout_pipe[0]);
            libc::close(stderr_pipe[0]);
        }

        // Open container root directory before switching mount namespace
        let root_path = format!("/proc/{}/root", container_pid);
        let root_c = CString::new(root_path.as_str()).unwrap();
        let root_fd = unsafe { libc::open(root_c.as_ptr(), libc::O_RDONLY) };

        let ns_list = [
            (format!("/proc/{}/ns/ipc", container_pid), libc::CLONE_NEWIPC),
            (format!("/proc/{}/ns/uts", container_pid), libc::CLONE_NEWUTS),
            (format!("/proc/{}/ns/net", container_pid), libc::CLONE_NEWNET),
            (format!("/proc/{}/ns/cgroup", container_pid), libc::CLONE_NEWCGROUP),
            (format!("/proc/{}/ns/pid", container_pid), libc::CLONE_NEWPID),
            (format!("/proc/{}/ns/mnt", container_pid), libc::CLONE_NEWNS),
        ];

        for (path, nstype) in &ns_list {
            let path_c = CString::new(path.as_str()).unwrap();
            let fd = unsafe { libc::open(path_c.as_ptr(), libc::O_RDONLY) };
            if fd >= 0 {
                let ret = unsafe { libc::setns(fd, *nstype) };
                unsafe { libc::close(fd) };
                if ret != 0 {
                    let err_msg = format!("Failed to setns {}: {}\n", path, std::io::Error::last_os_error());
                    unsafe {
                        libc::write(stderr_pipe[1], err_msg.as_ptr() as *const _, err_msg.len());
                        libc::_exit(125);
                    }
                }
            }
        }

        // Pivot/chroot into container root directory
        if root_fd >= 0 {
            unsafe {
                libc::fchdir(root_fd);
                libc::chroot(b".\0".as_ptr());
                libc::close(root_fd);
            }
        }

        // Fork the child that will actually execute within the container PID & Mount namespaces
        let exec_child_pid = unsafe { libc::fork() };
        if exec_child_pid < 0 {
            unsafe { libc::_exit(125) };
        }

        if exec_child_pid == 0 {
            // Inside the container!
            unsafe {
                libc::dup2(stdout_pipe[1], libc::STDOUT_FILENO);
                libc::dup2(stderr_pipe[1], libc::STDERR_FILENO);
                libc::close(stdout_pipe[1]);
                libc::close(stderr_pipe[1]);
                let slash_c = CString::new("/").unwrap();
                libc::chdir(slash_c.as_ptr());
            }

            let cmd_cfg = ContainerConfig {
                id: format!("exec-{}", container_pid),
                hostname: String::new(),
                rootfs: PathBuf::from("/"),
                command: command.to_string(),
                args: args.to_vec(),
                env: env.clone(),
                cgroup_path: None,
                netns_enabled: false,
                assigned_ip: None,
                ports: vec![],
                read_only: false,
                drop_caps: vec![],
            };
            execute_payload(&cmd_cfg);
            unsafe { libc::_exit(127) };
        }

        // Helper waits for exec_child
        unsafe {
            libc::close(stdout_pipe[1]);
            libc::close(stderr_pipe[1]);
        }
        let mut status: i32 = 0;
        unsafe {
            libc::waitpid(exec_child_pid, &mut status, 0);
        }
        let code = if libc::WIFEXITED(status) {
            libc::WEXITSTATUS(status)
        } else if libc::WIFSIGNALED(status) {
            128 + libc::WTERMSIG(status)
        } else {
            1
        };
        unsafe { libc::_exit(code) };
    }

    // Parent
    unsafe {
        libc::close(stdout_pipe[1]);
        libc::close(stderr_pipe[1]);
    }

    let mut stdout_file = unsafe { std::fs::File::from_raw_fd(stdout_pipe[0]) };
    let mut stderr_file = unsafe { std::fs::File::from_raw_fd(stderr_pipe[0]) };

    let mut stdout_buf = Vec::new();
    let mut stderr_buf = Vec::new();
    let _ = stdout_file.read_to_end(&mut stdout_buf);
    let _ = stderr_file.read_to_end(&mut stderr_buf);

    let mut status: i32 = 0;
    unsafe {
        libc::waitpid(helper_pid, &mut status, 0);
    }
    let exit_code = if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else if libc::WIFSIGNALED(status) {
        128 + libc::WTERMSIG(status)
    } else {
        -1
    };

    Ok(ExecutionResult {
        exit_code,
        stdout: String::from_utf8_lossy(&stdout_buf).into_owned(),
        stderr: String::from_utf8_lossy(&stderr_buf).into_owned(),
    })
}

/// Sets up genuine Linux namespaces, mounts rootfs, pivot_root, procfs, devtmpfs,
/// and executes the container command.
pub fn run_in_isolated_namespaces(config: &ContainerConfig) -> Result<ExecutionResult> {
    #[cfg(target_os = "linux")]
    return run_linux_namespaces(config);

    #[cfg(not(target_os = "linux"))]
    {
        let _ = config;
        Err(MacletError::Guest(
            "Linux namespaces are only supported on Linux target".to_string(),
        ))
    }
}

#[cfg(target_os = "linux")]
fn run_linux_namespaces(config: &ContainerConfig) -> Result<ExecutionResult> {
    use std::io::Read;
    use std::os::unix::io::FromRawFd;

    // Create pipes for stdout and stderr to capture container output
    let mut stdout_pipe = [0i32; 2];
    let mut stderr_pipe = [0i32; 2];
    unsafe {
        if libc::pipe(stdout_pipe.as_mut_ptr()) != 0 || libc::pipe(stderr_pipe.as_mut_ptr()) != 0 {
            return Err(MacletError::Io(std::io::Error::last_os_error()));
        }
    }

    let intermediate_pid = unsafe { libc::fork() };
    if intermediate_pid < 0 {
        return Err(MacletError::Io(std::io::Error::last_os_error()));
    }

    if intermediate_pid == 0 {
        // --- Intermediate Process ---
        // Close read ends of pipes
        unsafe {
            libc::close(stdout_pipe[0]);
            libc::close(stderr_pipe[0]);
        }

        // Unshare Mount, UTS, IPC, and PID namespaces
        let clone_flags = libc::CLONE_NEWNS
            | libc::CLONE_NEWUTS
            | libc::CLONE_NEWIPC
            | libc::CLONE_NEWPID;

        let ret = unsafe { libc::unshare(clone_flags) };
        if ret != 0 {
            let err = std::io::Error::last_os_error();
            let msg = format!("Failed to unshare namespaces: {}\n", err);
            unsafe {
                libc::write(
                    stderr_pipe[1],
                    msg.as_ptr() as *const libc::c_void,
                    msg.len(),
                );
                libc::_exit(1);
            }
        }

        let mut sync_pipe = [0i32; 2];
        if config.netns_enabled {
            unsafe {
                if libc::pipe(sync_pipe.as_mut_ptr()) != 0 {
                    libc::_exit(1);
                }
            }
        }

        // Fork the container init process (which becomes PID 1 in the new PID namespace!)
        let container_pid = unsafe { libc::fork() };
        if container_pid < 0 {
            let err = std::io::Error::last_os_error();
            let msg = format!("Failed to fork container init process: {}\n", err);
            unsafe {
                libc::write(
                    stderr_pipe[1],
                    msg.as_ptr() as *const libc::c_void,
                    msg.len(),
                );
                libc::_exit(1);
            }
        }

        if container_pid == 0 {
            if config.netns_enabled {
                unsafe {
                    let _ = libc::unshare(libc::CLONE_NEWNET);
                    libc::close(sync_pipe[1]);
                    let mut b = 0u8;
                    libc::read(sync_pipe[0], &mut b as *mut u8 as *mut libc::c_void, 1);
                    libc::close(sync_pipe[0]);
                }
            }

            // Attach to cgroup v2 if specified
            if let Some(ref cg) = config.cgroup_path {
                let procs_file = cg.join("cgroup.procs");
                let _ = fs::write(procs_file, "0\n");
                // Unshare cgroup namespace so the container's cgroup becomes the virtualized root
                unsafe {
                    let _ = libc::unshare(libc::CLONE_NEWCGROUP);
                }
            }

            // Redirect stdout and stderr to pipes
            unsafe {
                libc::dup2(stdout_pipe[1], libc::STDOUT_FILENO);
                libc::dup2(stderr_pipe[1], libc::STDERR_FILENO);
                libc::close(stdout_pipe[1]);
                libc::close(stderr_pipe[1]);
            }

            // Execute container isolation setup
            if let Err(e) = setup_container_root(config) {
                eprintln!("[Container Init Error] {}", e);
                unsafe { libc::_exit(126) };
            }

            // Launch container payload
            execute_payload(config);
            unsafe { libc::_exit(127) };
        }

        // Intermediate supervisor: configure network if enabled
        if config.netns_enabled {
            unsafe {
                libc::close(sync_pipe[0]);
            }
            if let Some(ip) = config.assigned_ip {
                let net_mgr = maclet_network::NetworkManager::new();
                let _ = net_mgr.init_host_network();
                let _ = net_mgr.setup_container_network(&config.id, container_pid as u32, ip);
                for p in &config.ports {
                    if let Some((hp, cp)) = p.split_once(':') {
                        if let (Ok(h_port), Ok(c_port)) = (hp.parse::<u16>(), cp.parse::<u16>()) {
                            let _ = net_mgr.forward_port(h_port, ip, c_port);
                        }
                    }
                }
            }
            unsafe {
                let b = 1u8;
                libc::write(sync_pipe[1], &b as *const u8 as *const libc::c_void, 1);
                libc::close(sync_pipe[1]);
            }
        }

        // Intermediate supervisor: wait for container init process
        unsafe {
            libc::close(stdout_pipe[1]);
            libc::close(stderr_pipe[1]);

            let mut status = 0i32;
            libc::waitpid(container_pid, &mut status, 0);

            if config.netns_enabled {
                let net_mgr = maclet_network::NetworkManager::new();
                let _ = net_mgr.teardown_container_network(&config.id);
            }

            let exit_code = if libc::WIFEXITED(status) {
                libc::WEXITSTATUS(status)
            } else if libc::WIFSIGNALED(status) {
                128 + libc::WTERMSIG(status)
            } else {
                1
            };

            libc::_exit(exit_code);
        }
    }

    // --- Host/Agent Process ---
    // Close write ends of pipes
    unsafe {
        libc::close(stdout_pipe[1]);
        libc::close(stderr_pipe[1]);
    }

    let mut stdout_file = unsafe { fs::File::from_raw_fd(stdout_pipe[0]) };
    let mut stderr_file = unsafe { fs::File::from_raw_fd(stderr_pipe[0]) };

    let mut stdout_buf = Vec::new();
    let mut stderr_buf = Vec::new();

    stdout_file.read_to_end(&mut stdout_buf)?;
    stderr_file.read_to_end(&mut stderr_buf)?;

    let mut status = 0i32;
    unsafe {
        libc::waitpid(intermediate_pid, &mut status, 0);
    }

    let exit_code = if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else if libc::WIFSIGNALED(status) {
        128 + libc::WTERMSIG(status)
    } else {
        1
    };

    Ok(ExecutionResult {
        exit_code,
        stdout: String::from_utf8_lossy(&stdout_buf).into_owned(),
        stderr: String::from_utf8_lossy(&stderr_buf).into_owned(),
    })
}

#[cfg(target_os = "linux")]
fn setup_container_root(config: &ContainerConfig) -> Result<()> {
    // 1. Set hostname in UTS namespace
    let host_c = CString::new(config.hostname.as_str())
        .map_err(|e| MacletError::Guest(format!("Hostname CString error: {e}")))?;
    unsafe {
        if libc::sethostname(host_c.as_ptr(), config.hostname.len()) != 0 {
            return Err(MacletError::Guest(format!(
                "Failed to sethostname: {}",
                std::io::Error::last_os_error()
            )));
        }
    }

    // 2. Make all mounts in this mount namespace private
    unsafe {
        let none: *const libc::c_char = std::ptr::null();
        let root_c = CString::new("/").unwrap();
        libc::mount(
            none,
            root_c.as_ptr(),
            none,
            libc::MS_REC | libc::MS_PRIVATE,
            std::ptr::null(),
        );
    }

    // 3. Create a clean tmpfs staging area at /mnt to serve as intermediate parent mount
    let _ = fs::create_dir_all("/mnt");
    let mnt_c = CString::new("/mnt").unwrap();
    let tmpfs_type = CString::new("tmpfs").unwrap();
    unsafe {
        if libc::mount(
            tmpfs_type.as_ptr(),
            mnt_c.as_ptr(),
            tmpfs_type.as_ptr(),
            0,
            std::ptr::null(),
        ) != 0
        {
            return Err(MacletError::Guest(format!(
                "Failed to mount tmpfs on /mnt: {}",
                std::io::Error::last_os_error()
            )));
        }
    }

    // 4. Create container rootfs directory inside /mnt and bind mount
    let target_root = Path::new("/mnt/target_root");
    fs::create_dir_all(target_root)?;
    let target_root_c = CString::new("/mnt/target_root").unwrap();
    let src_rootfs_c = CString::new(config.rootfs.to_str().unwrap()).unwrap();

    unsafe {
        let none: *const libc::c_char = std::ptr::null();
        if libc::mount(
            src_rootfs_c.as_ptr(),
            target_root_c.as_ptr(),
            none,
            libc::MS_BIND | libc::MS_REC,
            std::ptr::null(),
        ) != 0
        {
            return Err(MacletError::Guest(format!(
                "Failed to bind mount rootfs to /mnt/target_root: {}",
                std::io::Error::last_os_error()
            )));
        }
    }

    // 5. Chroot into /mnt so current root is a real mountpoint (NOT rootfs)
    unsafe {
        let slash_c = CString::new("/").unwrap();
        if libc::chroot(mnt_c.as_ptr()) != 0 || libc::chdir(slash_c.as_ptr()) != 0 {
            return Err(MacletError::Guest(format!(
                "Failed to chroot into /mnt: {}",
                std::io::Error::last_os_error()
            )));
        }
    }

    // 6. Now inside /mnt:
    // new_root is "/target_root"
    // put_old is "/target_root/.old_root"
    let old_root_dir = Path::new("/target_root/.old_root");
    fs::create_dir_all(old_root_dir)?;
    let new_root_c = CString::new("/target_root").unwrap();
    let put_old_c = CString::new("/target_root/.old_root").unwrap();

    // 7. Invoke pivot_root("/target_root", "/target_root/.old_root")
    unsafe {
        if pivot_root_syscall(new_root_c.as_ptr(), put_old_c.as_ptr()) != 0 {
            return Err(MacletError::Guest(format!(
                "pivot_root failed: {}",
                std::io::Error::last_os_error()
            )));
        }
    }

    // 8. Change working directory to new root "/"
    let slash_c = CString::new("/").unwrap();
    unsafe {
        libc::chdir(slash_c.as_ptr());
    }

    // 9. Mount /proc inside container (specific to this PID namespace!)
    fs::create_dir_all("/proc")?;
    let proc_c = CString::new("/proc").unwrap();
    let proc_type = CString::new("proc").unwrap();
    unsafe {
        let _ = libc::mount(
            proc_type.as_ptr(),
            proc_c.as_ptr(),
            proc_type.as_ptr(),
            libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC,
            std::ptr::null(),
        );
    }

    // 10. Mount /sys
    fs::create_dir_all("/sys")?;
    let sys_c = CString::new("/sys").unwrap();
    let sys_type = CString::new("sysfs").unwrap();
    unsafe {
        let _ = libc::mount(
            sys_type.as_ptr(),
            sys_c.as_ptr(),
            sys_type.as_ptr(),
            libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC | libc::MS_RDONLY,
            std::ptr::null(),
        );
    }

    // 10b. Mount cgroup2 on /sys/fs/cgroup
    let _ = fs::create_dir_all("/sys/fs/cgroup");
    let cgroup_c = CString::new("/sys/fs/cgroup").unwrap();
    let cgroup_type = CString::new("cgroup2").unwrap();
    unsafe {
        let _ = libc::mount(
            cgroup_type.as_ptr(),
            cgroup_c.as_ptr(),
            cgroup_type.as_ptr(),
            libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC,
            std::ptr::null(),
        );
    }

    // 11. Mount /dev
    fs::create_dir_all("/dev")?;
    let dev_c = CString::new("/dev").unwrap();
    let dev_type = CString::new("devtmpfs").unwrap();
    unsafe {
        let _ = libc::mount(
            dev_type.as_ptr(),
            dev_c.as_ptr(),
            dev_type.as_ptr(),
            libc::MS_NOSUID,
            std::ptr::null(),
        );
    }

    // 12. Mount private /tmp
    fs::create_dir_all("/tmp")?;
    let tmp_c = CString::new("/tmp").unwrap();
    let tmpfs_type = CString::new("tmpfs").unwrap();
    unsafe {
        let _ = libc::mount(
            tmpfs_type.as_ptr(),
            tmp_c.as_ptr(),
            tmpfs_type.as_ptr(),
            libc::MS_NOSUID | libc::MS_NODEV,
            std::ptr::null(),
        );
    }

    // 13. Unmount and remove old root
    unsafe {
        let old_root_path = CString::new("/.old_root").unwrap();
        libc::umount2(old_root_path.as_ptr(), libc::MNT_DETACH);
    }
    let _ = fs::remove_dir("/.old_root");

    // 14. If read_only is requested, remount / as read-only
    if config.read_only {
        let none: *const libc::c_char = std::ptr::null();
        let slash_c = CString::new("/").unwrap();
        unsafe {
            let _ = libc::mount(
                none,
                slash_c.as_ptr(),
                none,
                libc::MS_BIND | libc::MS_REMOUNT | libc::MS_RDONLY,
                std::ptr::null(),
            );
        }
    }

    Ok(())
}

#[cfg(target_os = "linux")]
fn cap_name_to_num(cap: &str) -> Option<libc::c_ulong> {
    let cap_upper = cap.trim().to_uppercase();
    match cap_upper.as_str() {
        "CAP_CHOWN" | "CHOWN" => Some(0),
        "CAP_DAC_OVERRIDE" | "DAC_OVERRIDE" => Some(1),
        "CAP_DAC_READ_SEARCH" | "DAC_READ_SEARCH" => Some(2),
        "CAP_FOWNER" | "FOWNER" => Some(3),
        "CAP_FSETID" | "FSETID" => Some(4),
        "CAP_KILL" | "KILL" => Some(5),
        "CAP_SETGID" | "SETGID" => Some(6),
        "CAP_SETUID" | "SETUID" => Some(7),
        "CAP_SETPCAP" | "SETPCAP" => Some(8),
        "CAP_LINUX_IMMUTABLE" | "LINUX_IMMUTABLE" => Some(9),
        "CAP_NET_BIND_SERVICE" | "NET_BIND_SERVICE" => Some(10),
        "CAP_NET_BROADCAST" | "NET_BROADCAST" => Some(11),
        "CAP_NET_ADMIN" | "NET_ADMIN" => Some(12),
        "CAP_NET_RAW" | "NET_RAW" => Some(13),
        "CAP_IPC_LOCK" | "IPC_LOCK" => Some(14),
        "CAP_IPC_OWNER" | "IPC_OWNER" => Some(15),
        "CAP_SYS_MODULE" | "SYS_MODULE" => Some(16),
        "CAP_SYS_RAWIO" | "SYS_RAWIO" => Some(17),
        "CAP_SYS_CHROOT" | "SYS_CHROOT" => Some(18),
        "CAP_SYS_PTRACE" | "SYS_PTRACE" => Some(19),
        "CAP_SYS_PACCT" | "SYS_PACCT" => Some(20),
        "CAP_SYS_ADMIN" | "SYS_ADMIN" => Some(21),
        "CAP_SYS_BOOT" | "SYS_BOOT" => Some(22),
        "CAP_SYS_NICE" | "SYS_NICE" => Some(23),
        "CAP_SYS_RESOURCE" | "SYS_RESOURCE" => Some(24),
        "CAP_SYS_TIME" | "SYS_TIME" => Some(25),
        "CAP_SYS_TTY_CONFIG" | "SYS_TTY_CONFIG" => Some(26),
        "CAP_MKNOD" | "MKNOD" => Some(27),
        "CAP_LEASE" | "LEASE" => Some(28),
        "CAP_AUDIT_WRITE" | "AUDIT_WRITE" => Some(29),
        "CAP_AUDIT_CONTROL" | "AUDIT_CONTROL" => Some(30),
        "CAP_SETFCAP" | "SETFCAP" => Some(31),
        "CAP_MAC_OVERRIDE" | "MAC_OVERRIDE" => Some(32),
        "CAP_MAC_ADMIN" | "MAC_ADMIN" => Some(33),
        "CAP_SYSLOG" | "SYSLOG" => Some(34),
        "CAP_WAKE_ALARM" | "WAKE_ALARM" => Some(35),
        "CAP_BLOCK_SUSPEND" | "BLOCK_SUSPEND" => Some(36),
        "CAP_AUDIT_READ" | "AUDIT_READ" => Some(37),
        _ => cap.parse::<libc::c_ulong>().ok(),
    }
}

#[cfg(target_os = "linux")]
unsafe fn apply_security_boundaries(config: &ContainerConfig) {
    // 1. Enforce PR_SET_NO_NEW_PRIVS (prevents child processes from acquiring new privileges)
    libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0);

    // 2. Drop capability bounding set
    let caps_to_drop = if config.drop_caps.is_empty() {
        // Standard security hardening defaults
        vec![
            16, // CAP_SYS_MODULE
            17, // CAP_SYS_RAWIO
            21, // CAP_SYS_ADMIN
            22, // CAP_SYS_BOOT
            30, // CAP_AUDIT_CONTROL
        ]
    } else {
        config
            .drop_caps
            .iter()
            .filter_map(|c| cap_name_to_num(c))
            .collect()
    };

    for cap in caps_to_drop {
        libc::prctl(libc::PR_CAPBSET_DROP, cap, 0, 0, 0);
    }
}

#[cfg(target_os = "linux")]
fn execute_payload(config: &ContainerConfig) {
    // Apply Linux security boundaries (No-new-privs & drop bounding caps)
    unsafe {
        apply_security_boundaries(config);
    }

    let resolved_path = if config.command.contains('/') {
        PathBuf::from(&config.command)
    } else {
        let path_var = config
            .env
            .get("PATH")
            .cloned()
            .unwrap_or_else(|| "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".to_string());
        let mut found = PathBuf::from(&config.command);
        for dir in path_var.split(':') {
            let candidate = Path::new(dir).join(&config.command);
            if candidate.exists() {
                found = candidate;
                break;
            }
        }
        found
    };

    let cmd_c = match CString::new(resolved_path.to_string_lossy().as_bytes()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Invalid command string: {}", e);
            return;
        }
    };

    let mut argv: Vec<CString> = Vec::new();
    argv.push(cmd_c.clone());
    for arg in &config.args {
        if let Ok(a) = CString::new(arg.as_str()) {
            argv.push(a);
        }
    }
    let mut argv_ptrs: Vec<*const libc::c_char> = argv.iter().map(|s| s.as_ptr()).collect();
    argv_ptrs.push(std::ptr::null());

    let mut envp: Vec<CString> = Vec::new();
    for (k, v) in &config.env {
        if let Ok(entry) = CString::new(format!("{}={}", k, v)) {
            envp.push(entry);
        }
    }
    // Default PATH if not set
    if !config.env.contains_key("PATH") {
        envp.push(
            CString::new("PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin")
                .unwrap(),
        );
    }
    let mut envp_ptrs: Vec<*const libc::c_char> = envp.iter().map(|s| s.as_ptr()).collect();
    envp_ptrs.push(std::ptr::null());

    unsafe {
        libc::execve(cmd_c.as_ptr(), argv_ptrs.as_ptr(), envp_ptrs.as_ptr());
    }

    eprintln!(
        "Failed to execve '{}': {}",
        config.command,
        std::io::Error::last_os_error()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_container_config() {
        let cfg = ContainerConfig {
            id: "test-1".to_string(),
            hostname: "maclet-test".to_string(),
            rootfs: PathBuf::from("/tmp/rootfs"),
            command: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), "echo hi".to_string()],
            env: HashMap::new(),
            cgroup_path: Some(PathBuf::from("/sys/fs/cgroup/maclet/test-1")),
            netns_enabled: true,
            assigned_ip: None,
            ports: vec!["8080:80".to_string()],
            read_only: false,
            drop_caps: vec!["CAP_SYS_ADMIN".to_string()],
        };
        assert_eq!(cfg.id, "test-1");
        assert_eq!(cfg.hostname, "maclet-test");
        assert!(cfg.netns_enabled);
    }
}
