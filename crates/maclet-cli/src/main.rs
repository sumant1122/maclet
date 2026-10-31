mod cpio;
mod initramfs;
mod kernel;

use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use colored::Colorize;
use maclet_common::{GuestMessage, HostMessage, VmConfig};
use maclet_ipc::{read_async, write_async};
use maclet_vm::VirtualMachine;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Mutex;

#[derive(Parser)]
#[command(name = "maclet")]
#[command(about = "A native macOS container runtime written in Rust", long_about = None)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Manage the Maclet Linux Virtual Machine
    Machine {
        #[command(subcommand)]
        action: MachineCommands,
    },

    /// Execute a command in a running container or the Linux VM
    #[command(trailing_var_arg = true)]
    Exec {
        /// Target container ID (or command to execute in VM)
        target: String,

        /// Command to execute inside container (if target is container)
        #[arg(allow_hyphen_values = true)]
        command: Option<String>,

        /// Arguments for the command
        #[arg(allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Pull an OCI image from registry (Docker Hub, GHCR)
    Pull {
        /// Image reference (e.g. ubuntu:24.04, alpine:latest, ghcr.io/...)
        image: String,
    },

    /// List local container images
    Images,

    /// Run an isolated Linux container with genuine namespaces, cgroups, and rootfs
    #[command(trailing_var_arg = true)]
    Run {
        /// Run container in background and print container ID
        #[arg(short = 'd', long = "detach")]
        detach: bool,

        /// Assign a name to the container
        #[arg(long = "name")]
        name: Option<String>,

        /// Image or command to execute (e.g. ubuntu:24.04, alpine:latest, /bin/sh)
        image: String,

        /// Optional command to execute inside container
        #[arg(allow_hyphen_values = true)]
        command: Option<String>,

        /// Arguments for the command
        #[arg(allow_hyphen_values = true)]
        args: Vec<String>,

        /// Custom hostname for the container
        #[arg(long)]
        hostname: Option<String>,

        /// Memory limit (e.g. 512m, 1g, 256m)
        #[arg(long)]
        memory: Option<String>,

        /// CPU limit in number of vCPUs (e.g. 1.0, 0.5, 2.0)
        #[arg(long)]
        cpus: Option<f64>,

        /// Maximum number of concurrent processes
        #[arg(long = "pids-limit")]
        pids_limit: Option<u64>,

        /// Publish a container's port(s) to the host (e.g. -p 8080:80)
        #[arg(short = 'p', long = "publish")]
        publish: Vec<String>,

        /// Mount the container's root filesystem as read only
        #[arg(long = "read-only")]
        read_only: bool,

        /// Drop Linux capabilities (e.g. CAP_SYS_ADMIN, CAP_SYS_MODULE)
        #[arg(long = "cap-drop")]
        cap_drop: Vec<String>,

        /// Display cgroup v2 resource usage statistics upon completion
        #[arg(long)]
        stats: bool,
    },

    /// List containers
    Ps {
        /// Show all containers (default shows just running)
        #[arg(short = 'a', long = "all")]
        all: bool,
    },

    /// Stop one or more running containers
    Stop {
        /// Container ID or name
        id: String,

        /// Seconds to wait before killing the container
        #[arg(short = 't', long = "time", default_value_t = 10)]
        time: u64,
    },

    /// Remove one or more containers
    Rm {
        /// Container ID or name
        id: String,

        /// Force removal of a running container
        #[arg(short = 'f', long = "force")]
        force: bool,
    },

    /// Fetch the logs of a container
    Logs {
        /// Container ID or name
        id: String,

        /// Number of lines to show from the end of the logs
        #[arg(short = 'n', long = "tail")]
        tail: Option<usize>,
    },

    /// Inspect details and configuration of a container
    Inspect {
        /// Container identifier
        id: String,
    },

    /// Display live resource usage statistics for containers
    Stats {
        /// Container identifier (optional; defaults to all running containers)
        id: Option<String>,
    },

    /// Manage Maclet system and host information
    System {
        #[command(subcommand)]
        command: SystemCommands,
    },

    /// Display version and environment information
    Version,
}

#[derive(Subcommand)]
enum SystemCommands {
    /// Display system and runtime information
    Info,
}

#[derive(Subcommand)]
enum MachineCommands {
    /// Initialize Maclet environment, kernel, and guest agent
    Init {
        #[arg(long, default_value_t = 2)]
        cpus: usize,

        #[arg(long, default_value_t = 1024)]
        memory: u64,
    },

    /// Start the Maclet Linux VM
    Start {
        /// Run in daemon / background mode
        #[arg(short, long)]
        daemon: bool,
    },

    /// Stop the running Maclet VM
    Stop,

    /// Inspect the status of the Maclet VM
    Status,
}

fn maclet_home() -> PathBuf {
    dirs_home().join(".maclet")
}

fn dirs_home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
}

fn socket_path() -> PathBuf {
    maclet_home().join("maclet.sock")
}

fn config_path() -> PathBuf {
    maclet_home().join("config.json")
}

fn load_config() -> Result<VmConfig> {
    let p = config_path();
    if !p.exists() {
        return Err(anyhow!(
            "Maclet VM not initialized. Please run 'maclet machine init' first."
        ));
    }
    let data = fs::read_to_string(&p)?;
    let cfg = serde_json::from_str(&data)?;
    Ok(cfg)
}

fn find_workspace_root() -> Result<PathBuf> {
    let mut cur = std::env::current_dir()?;
    loop {
        if cur.join("Cargo.toml").exists() && cur.join("crates").exists() {
            return Ok(cur);
        }
        if !cur.pop() {
            break;
        }
    }
    // Fallback to current dir
    Ok(std::env::current_dir()?)
}

async fn handle_init(cpus: usize, memory: u64) -> Result<()> {
    println!("{}", "=== Initializing Maclet ===".bold().cyan());
    let base_dir = maclet_home();
    let kernel_dir = base_dir.join("kernel");
    fs::create_dir_all(&kernel_dir)?;

    let kernel_path = kernel::ensure_kernel(&kernel_dir)?;
    println!("Kernel ready at: {}", kernel_path.display().to_string().green());

    let modloop_path = kernel::ensure_modloop(&kernel_dir)?;
    println!("Kernel modules ready at: {}", modloop_path.display().to_string().green());

    let modules_dir = kernel::ensure_essential_modules(&kernel_dir)?;

    let rootfs_tar = initramfs::ensure_rootfs_tar(&kernel_dir)?;
    println!("Base rootfs ready at: {}", rootfs_tar.display().to_string().green());

    let workspace_root = find_workspace_root()?;
    let guest_bin = initramfs::build_guest_agent(&workspace_root)?;
    let initramfs_path = kernel_dir.join("initrd.img");
    initramfs::create_initramfs(&guest_bin, &rootfs_tar, Some(&modules_dir), &initramfs_path)?;

    let config = VmConfig {
        cpu_count: cpus,
        memory_mb: memory,
        kernel_path,
        initramfs_path,
        cmdline: "console=hvc0 rdinit=/init quiet".to_string(),
        disk_path: Some(modloop_path),
        enable_nat: true,
    };

    let config_json = serde_json::to_string_pretty(&config)?;
    fs::write(config_path(), config_json)?;
    println!(
        "Configuration saved to {}",
        config_path().display().to_string().green()
    );
    println!(
        "{}",
        "Maclet initialized successfully! Run 'maclet machine start' to boot Linux."
            .bold()
            .green()
    );
    Ok(())
}

async fn handle_start(daemon: bool) -> Result<()> {
    let sock_p = socket_path();
    if sock_p.exists() {
        if let Ok(_) = UnixStream::connect(&sock_p).await {
            println!("{}", "Maclet machine is already running.".yellow());
            return Ok(());
        } else {
            let _ = fs::remove_file(&sock_p);
        }
    }

    if daemon {
        let log_path = maclet_home().join("daemon.log");
        let log_file = fs::File::create(&log_path)?;
        let err_file = log_file.try_clone()?;
        let current_exe = std::env::current_exe()?;
        let child = std::process::Command::new(current_exe)
            .args(["machine", "start"])
            .stdin(std::process::Stdio::null())
            .stdout(log_file)
            .stderr(err_file)
            .spawn()
            .context("Failed to spawn daemon process")?;

        let pid_path = maclet_home().join("maclet.pid");
        let _ = fs::write(&pid_path, child.id().to_string());

        println!(
            "{}",
            format!("Maclet VM daemon started in background (PID {})", child.id())
                .bold()
                .green()
        );

        // Wait up to 5 seconds for socket to appear
        for _ in 0..50 {
            if sock_p.exists() {
                if UnixStream::connect(&sock_p).await.is_ok() {
                    println!("{}", "Maclet Linux VM is ready.".green());
                    return Ok(());
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        return Err(anyhow!("Timeout waiting for VM daemon socket to become ready"));
    }

    let config = load_config()?;
    println!("{}", "Booting Maclet Linux Virtual Machine...".cyan());

    let (mut vm, guest_channel) = VirtualMachine::start(&config, None)
        .context("Failed to start virtual machine")?;

    let (guest_reader, mut guest_writer) = guest_channel.into_split();
    let mut guest_buf_reader = tokio::io::BufReader::new(guest_reader);

    // Initial handshake
    println!("Waiting for guest agent handshake...");
    let ready_msg: Option<GuestMessage> = read_async(&mut guest_buf_reader).await?;
    match ready_msg {
        Some(GuestMessage::Ready { version, arch, uptime_secs: _ }) => {
            println!(
                "{}",
                format!("Guest agent online: version {}, arch {}", version, arch).green()
            );
        }
        other => {
            return Err(anyhow!("Unexpected initial message from guest: {:?}", other));
        }
    }

    // Ping test
    write_async(&mut guest_writer, &HostMessage::Ping { id: "init-ping".to_string() }).await?;
    let mut pong_received = false;
    for _ in 0..5 {
        if let Some(msg) = read_async::<_, GuestMessage>(&mut guest_buf_reader).await? {
            if let GuestMessage::Pong { id } = msg {
                if id == "init-ping" {
                    pong_received = true;
                    break;
                }
            }
        }
    }
    if !pong_received {
        return Err(anyhow!("Failed to receive pong from guest agent"));
    }

    // Bind Unix domain socket
    if sock_p.exists() {
        let _ = fs::remove_file(&sock_p);
    }
    let listener = UnixListener::bind(&sock_p)?;
    println!(
        "Host control socket listening at: {}",
        sock_p.display().to_string().cyan()
    );
    println!("{}", "Maclet Linux VM is running and ready for commands.".bold().green());

    let guest_buf_reader = Arc::new(Mutex::new(guest_buf_reader));
    let guest_writer = Arc::new(Mutex::new(guest_writer));

    // Listen loop for incoming CLI connections
    loop {
        tokio::select! {
            accept_res = listener.accept() => {
                let (client_sock, _) = match accept_res {
                    Ok(res) => res,
                    Err(e) => {
                        eprintln!("Socket accept error: {}", e);
                        continue;
                    }
                };

                let guest_r = Arc::clone(&guest_buf_reader);
                let guest_w = Arc::clone(&guest_writer);

                tokio::spawn(async move {
                    let (client_r, mut client_w) = client_sock.into_split();
                    let mut client_buf_r = tokio::io::BufReader::new(client_r);

                    while let Ok(Some(host_req)) = read_async::<_, HostMessage>(&mut client_buf_r).await {
                        let is_shutdown = matches!(host_req, HostMessage::Shutdown { .. });

                        let mut gw = guest_w.lock().await;
                        let mut gr = guest_r.lock().await;

                        if let Err(e) = write_async(&mut *gw, &host_req).await {
                            eprintln!("Failed to forward request to guest: {}", e);
                            break;
                        }

                        if is_shutdown {
                            let _ = write_async(&mut client_w, &GuestMessage::Pong { id: "stop".to_string() }).await;
                            tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
                            let _ = fs::remove_file(socket_path());
                            let _ = fs::remove_file(maclet_home().join("maclet.pid"));
                            std::process::exit(0);
                        }

                        match read_async::<_, GuestMessage>(&mut *gr).await {
                            Ok(Some(guest_res)) => {
                                let _ = write_async(&mut client_w, &guest_res).await;
                            }
                            Ok(None) => break,
                            Err(e) => {
                                eprintln!("Error reading response from guest: {}", e);
                                break;
                            }
                        }
                    }
                });
            }
            _ = tokio::signal::ctrl_c() => {
                println!("\nReceived Ctrl-C, stopping VM...");
                let _ = vm.stop();
                let _ = fs::remove_file(&sock_p);
                break;
            }
        }
    }

    Ok(())
}

async fn read_with_timeout<R, T>(r: &mut R, secs: u64) -> Result<Option<T>>
where
    R: tokio::io::AsyncBufRead + Unpin,
    T: serde::de::DeserializeOwned,
{
    match tokio::time::timeout(tokio::time::Duration::from_secs(secs), read_async(r)).await {
        Ok(res) => res.map_err(|e| anyhow!("IPC error: {}", e)),
        Err(_) => Err(anyhow!("Timed out waiting for response from Maclet VM")),
    }
}

async fn handle_status() -> Result<()> {
    let sock_p = socket_path();
    if !sock_p.exists() {
        println!("{}", "Status: Maclet VM is STOPPED (control socket not found)".yellow());
        return Ok(());
    }

    let stream = match UnixStream::connect(&sock_p).await {
        Ok(s) => s,
        Err(_) => {
            println!("{}", "Status: Maclet VM is STOPPED (socket stale)".yellow());
            let _ = fs::remove_file(&sock_p);
            return Ok(());
        }
    };

    let (r, mut w) = stream.into_split();
    let mut buf_r = tokio::io::BufReader::new(r);

    write_async(&mut w, &HostMessage::Ping { id: "status-check".to_string() }).await?;
    let resp: Option<GuestMessage> = match read_with_timeout(&mut buf_r, 5).await {
        Ok(r) => r,
        Err(e) => {
            println!("{}: {}", "Status: UNRESPONSIVE".bold().red(), e);
            return Ok(());
        }
    };

    if let Some(GuestMessage::Pong { id }) = resp {
        if id == "status-check" {
            println!("{}", "Status: Maclet VM is RUNNING (guest agent responding)".bold().green());
            return Ok(());
        }
    }

    println!("{}", "Status: UNKNOWN (guest agent did not respond properly)".red());
    Ok(())
}

async fn handle_machine_stop() -> Result<()> {
    let sock_p = socket_path();
    let pid_p = maclet_home().join("maclet.pid");

    let pid = if pid_p.exists() {
        fs::read_to_string(&pid_p)
            .ok()
            .and_then(|s| s.trim().parse::<i32>().ok())
    } else {
        None
    };

    if !sock_p.exists() && pid.is_none() {
        println!("{}", "Maclet VM is already stopped.".yellow());
        return Ok(());
    }

    if sock_p.exists() {
        if let Ok(stream) = UnixStream::connect(&sock_p).await {
            let (r, mut w) = stream.into_split();
            let mut buf_r = tokio::io::BufReader::new(r);

            println!("Sending shutdown command to guest VM...");
            let _ = write_async(&mut w, &HostMessage::Shutdown { id: "cli-stop".to_string() }).await;
            let _ = tokio::time::timeout(
                tokio::time::Duration::from_secs(3),
                read_async::<_, GuestMessage>(&mut buf_r),
            )
            .await;
        }
        let _ = fs::remove_file(&sock_p);
    }

    if let Some(p) = pid {
        unsafe {
            if libc::kill(p, 0) == 0 {
                libc::kill(p, libc::SIGTERM);
                for _ in 0..20 {
                    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                    if libc::kill(p, 0) != 0 {
                        break;
                    }
                }
                if libc::kill(p, 0) == 0 {
                    libc::kill(p, libc::SIGKILL);
                }
            }
        }
        let _ = fs::remove_file(&pid_p);
    }

    println!("{}", "Maclet Linux VM stopped successfully.".green());
    Ok(())
}

fn format_relative_time(created_at: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(created_at);
    let diff = now.saturating_sub(created_at);
    if diff < 60 {
        format!("{} seconds ago", diff)
    } else if diff < 3600 {
        format!("{} minutes ago", diff / 60)
    } else if diff < 86400 {
        format!("{} hours ago", diff / 3600)
    } else {
        format!("{} days ago", diff / 86400)
    }
}

async fn execute_single_command(
    w: &mut tokio::net::unix::OwnedWriteHalf,
    buf_r: &mut tokio::io::BufReader<tokio::net::unix::OwnedReadHalf>,
    target: &str,
    is_container: bool,
    command: &str,
    args: Vec<String>,
) -> Result<(i32, String, String)> {
    if is_container {
        let req = HostMessage::ExecInContainer {
            id: target.to_string(),
            command: command.to_string(),
            args,
            env: HashMap::new(),
        };
        write_async(w, &req).await?;
        let resp: Option<GuestMessage> = read_with_timeout(buf_r, 60).await?;
        match resp {
            Some(GuestMessage::ContainerExecResult {
                exit_code,
                stdout,
                stderr,
                ..
            }) => Ok((exit_code, stdout, stderr)),
            Some(GuestMessage::Error { message, .. }) => Ok((1, String::new(), message)),
            _ => Ok((1, String::new(), "No response from guest agent".to_string())),
        }
    } else {
        let req_id = format!(
            "exec-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_millis()
        );
        let req = HostMessage::Exec {
            id: req_id,
            command: command.to_string(),
            args,
            env: HashMap::new(),
            timeout_secs: 60,
        };
        write_async(w, &req).await?;
        let resp: Option<GuestMessage> = read_with_timeout(buf_r, 60).await?;
        match resp {
            Some(GuestMessage::ExecResult {
                exit_code,
                stdout,
                stderr,
                ..
            }) => Ok((exit_code, stdout, stderr)),
            Some(GuestMessage::Error { message, .. }) => Ok((1, String::new(), message)),
            _ => Ok((1, String::new(), "No response from guest agent".to_string())),
        }
    }
}

async fn run_interactive_shell(
    w: &mut tokio::net::unix::OwnedWriteHalf,
    buf_r: &mut tokio::io::BufReader<tokio::net::unix::OwnedReadHalf>,
    target: String,
    is_container: bool,
) -> Result<i32> {
    let display_name = if is_container {
        target.clone()
    } else {
        "maclet-vm".to_string()
    };

    println!(
        "{}",
        format!(
            "Connected to {} (/bin/sh). Type 'exit' to quit.",
            display_name
        )
        .bold()
        .green()
    );

    let mut cwd = "/".to_string();
    let mut rl = rustyline::DefaultEditor::new()?;

    loop {
        let prompt = format!("{}:{} # ", display_name.bold().green(), cwd.bold().blue());
        let readline = rl.readline(&prompt);
        match readline {
            Ok(line) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let _ = rl.add_history_entry(line.as_str());

                if trimmed == "exit" || trimmed == "quit" {
                    break;
                }

                // Handle directory change
                if trimmed.starts_with("cd ") || trimmed == "cd" {
                    let target_dir = if trimmed == "cd" {
                        "/root"
                    } else {
                        trimmed[3..].trim()
                    };

                    let eval_cmd = format!("cd {} && cd {} && pwd", cwd, target_dir);
                    let (code, stdout, stderr) = execute_single_command(
                        w,
                        buf_r,
                        &target,
                        is_container,
                        "/bin/sh",
                        vec!["-c".to_string(), eval_cmd],
                    )
                    .await?;

                    if code == 0 && !stdout.trim().is_empty() {
                        cwd = stdout.trim().to_string();
                    } else {
                        if !stderr.is_empty() {
                            eprint!("{}", stderr);
                        } else {
                            eprintln!("cd: {}: No such file or directory", target_dir);
                        }
                    }
                    continue;
                }

                // Execute command inside container
                let wrapped_cmd = format!("cd {} && {}", cwd, trimmed);
                let (_code, stdout, stderr) = execute_single_command(
                    w,
                    buf_r,
                    &target,
                    is_container,
                    "/bin/sh",
                    vec!["-c".to_string(), wrapped_cmd],
                )
                .await?;

                if !stdout.is_empty() {
                    print!("{}", stdout);
                }
                if !stderr.is_empty() {
                    eprint!("{}", stderr);
                }
            }
            Err(rustyline::error::ReadlineError::Interrupted) => {
                // Ctrl-C
                println!("^C");
            }
            Err(rustyline::error::ReadlineError::Eof) => {
                // Ctrl-D
                break;
            }
            Err(err) => {
                eprintln!("Error reading input: {:?}", err);
                break;
            }
        }
    }

    println!("Connection to {} closed.", display_name);
    Ok(0)
}

async fn handle_exec(target: String, command: Option<String>, args: Vec<String>) -> Result<i32> {
    let sock_p = socket_path();
    if !sock_p.exists() {
        return Err(anyhow!(
            "Maclet VM is not running. Start it with 'maclet machine start' first."
        ));
    }

    let stream = UnixStream::connect(&sock_p)
        .await
        .context("Failed to connect to Maclet VM control socket")?;

    let (r, mut w) = stream.into_split();
    let mut buf_r = tokio::io::BufReader::new(r);

    // Differentiate between container exec and VM exec
    let is_container = if target.starts_with("cnt-") {
        true
    } else {
        let req = HostMessage::ListContainers { all: true };
        write_async(&mut w, &req).await?;
        let resp: Option<GuestMessage> = read_with_timeout(&mut buf_r, 10).await?;
        if let Some(GuestMessage::ContainerList { containers }) = resp {
            containers.iter().any(|c| c.id == target || c.name == target || c.id.starts_with(&target))
        } else {
            false
        }
    };

    let is_interactive = command.is_none()
        || (matches!(
            command.as_deref(),
            Some("/bin/sh") | Some("/bin/bash") | Some("sh") | Some("bash")
        ) && args.is_empty());

    if is_interactive {
        return run_interactive_shell(&mut w, &mut buf_r, target, is_container).await;
    }

    if is_container {
        let exec_cmd = command.unwrap_or_else(|| "/bin/sh".to_string());
        let req = HostMessage::ExecInContainer {
            id: target.clone(),
            command: exec_cmd,
            args,
            env: HashMap::new(),
        };
        write_async(&mut w, &req).await?;
        let resp: Option<GuestMessage> = read_with_timeout(&mut buf_r, 30).await?;
        match resp {
            Some(GuestMessage::ContainerExecResult {
                exit_code,
                stdout,
                stderr,
                ..
            }) => {
                if !stdout.is_empty() {
                    print!("{}", stdout);
                }
                if !stderr.is_empty() {
                    eprint!("{}", stderr);
                }
                Ok(exit_code)
            }
            Some(GuestMessage::Error { message, .. }) => {
                eprintln!("{}: {}", "Exec error".bold().red(), message);
                Ok(1)
            }
            other => {
                eprintln!("Unexpected response from guest agent: {:?}", other);
                Ok(1)
            }
        }
    } else {
        let mut full_args = Vec::new();
        if let Some(c) = command {
            full_args.push(c);
        }
        full_args.extend(args);

        let req_id = format!(
            "exec-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_millis()
        );
        let req = HostMessage::Exec {
            id: req_id,
            command: target,
            args: full_args,
            env: HashMap::new(),
            timeout_secs: 30,
        };
        write_async(&mut w, &req).await?;
        let resp: Option<GuestMessage> = read_with_timeout(&mut buf_r, 30).await?;
        match resp {
            Some(GuestMessage::ExecResult {
                exit_code,
                stdout,
                stderr,
                ..
            }) => {
                if !stdout.is_empty() {
                    print!("{}", stdout);
                }
                if !stderr.is_empty() {
                    eprint!("{}", stderr);
                }
                Ok(exit_code)
            }
            Some(GuestMessage::Error { message, .. }) => {
                eprintln!("{}: {}", "Exec error".bold().red(), message);
                Ok(1)
            }
            other => {
                eprintln!("Unexpected response from guest: {:?}", other);
                Ok(1)
            }
        }
    }
}

async fn handle_pull(image: String) -> Result<i32> {
    let images_dir = dirs_home().join(".maclet/images");
    let mgr = maclet_image::ImageManager::new(images_dir)?;
    match mgr.pull(&image).await {
        Ok(img) => {
            println!("Successfully pulled image {} (digest: {})", img.name.bold(), img.id);
            Ok(0)
        }
        Err(e) => {
            eprintln!("{}: {}", "Pull error".bold().red(), e);
            Ok(1)
        }
    }
}

async fn handle_images() -> Result<i32> {
    let images_dir = dirs_home().join(".maclet/images");
    let mgr = maclet_image::ImageManager::new(images_dir)?;
    let list = mgr.list_images()?;
    println!(
        "{:<20} {:<12} {:<16} {:<14} {:<12}",
        "REPOSITORY", "TAG", "IMAGE ID", "CREATED", "SIZE"
    );
    for img in list {
        let size_mb = img.size_bytes as f64 / (1024.0 * 1024.0);
        let rel_time = format_relative_time(img.created_at);
        println!(
            "{:<20} {:<12} {:<16} {:<14} {:.1} MB",
            img.name, img.tag, img.id, rel_time, size_mb
        );
    }
    Ok(0)
}

async fn handle_system_info() -> Result<i32> {
    println!("{}", "=== Maclet Container Runtime System Information ===".bold().cyan());
    println!("{:<24} macOS (Darwin)", "Host OS:");
    println!("{:<24} {}", "Architecture:", std::env::consts::ARCH);
    println!("{:<24} Apple Virtualization Framework (VZVirtualMachine)", "Hypervisor:");
    println!("{:<24} 6.6.134-0-virt (ARM64)", "Linux Guest Kernel:");
    println!("{:<24} Namespaces, Cgroups v2, OverlayFS, pivot_root", "Container Engine:");
    println!("{:<24} OverlayFS (copy-on-write)", "Storage Driver:");
    println!("{:<24} maclet0 (10.88.0.1/16 bridge, veth pairs, NAT)", "Networking:");
    println!("{:<24} ~/.maclet/images", "Image Storage:");

    let sock_p = socket_path();
    if sock_p.exists() {
        if let Ok(stream) = UnixStream::connect(&sock_p).await {
            let (r, mut w) = stream.into_split();
            let mut buf_r = tokio::io::BufReader::new(r);
            let req = HostMessage::ListContainers { all: true };
            if write_async(&mut w, &req).await.is_ok() {
                if let Ok(Some(GuestMessage::ContainerList { containers })) = read_async(&mut buf_r).await {
                    let running = containers.iter().filter(|c| c.status == maclet_common::ContainerStatus::Running).count();
                    println!("{:<24} {} total ({} running)", "Containers:", containers.len(), running);
                    println!("{:<24} {}", "Daemon Status:", "Running".green());
                    return Ok(0);
                }
            }
        }
    }
    println!("{:<24} {}", "Daemon Status:", "Stopped".yellow());
    Ok(0)
}

async fn handle_run(
    detach: bool,
    name: Option<String>,
    image_or_cmd: String,
    command: Option<String>,
    args: Vec<String>,
    hostname: Option<String>,
    memory: Option<String>,
    cpus: Option<f64>,
    pids_limit: Option<u64>,
    publish: Vec<String>,
    read_only: bool,
    cap_drop: Vec<String>,
    show_stats: bool,
) -> Result<i32> {
    let sock_p = socket_path();
    if !sock_p.exists() {
        return Err(anyhow!(
            "Maclet VM is not running. Start it with 'maclet machine start' first."
        ));
    }

    let (image, cmd, final_args) = if image_or_cmd.starts_with('/') {
        let mut a = Vec::new();
        if let Some(c) = command {
            a.push(c);
        }
        a.extend(args);
        ("alpine:latest".to_string(), image_or_cmd, a)
    } else {
        let img = image_or_cmd;
        let c = command.unwrap_or_else(|| "/bin/sh".to_string());
        (img, c, args)
    };

    // Check if image exists locally; pull if missing
    let images_dir = dirs_home().join(".maclet/images");
    if let Ok(img_mgr) = maclet_image::ImageManager::new(images_dir) {
        if let Ok(images) = img_mgr.list_images() {
            let known = images.iter().any(|i| {
                i.name == image || format!("{}:{}", i.name, i.tag) == image || i.id == image
            });
            if !known && image != "alpine" && image != "alpine:latest" {
                println!("Image '{}' not found locally. Pulling from registry...", image);
                if let Err(e) = img_mgr.pull(&image).await {
                    eprintln!("Warning: Image pull failed: {}. Continuing with base rootfs.", e);
                }
            }
        }
    }

    let mut limits = maclet_common::ResourceLimits::new();
    if let Some(mem_str) = memory {
        let bytes = maclet_common::parse_memory_bytes(&mem_str)?;
        limits = limits.with_memory(bytes);
    }
    if let Some(cpu_val) = cpus {
        limits = limits.with_cpus(cpu_val);
    }
    if let Some(pids) = pids_limit {
        limits = limits.with_pids(pids);
    }

    let stream = UnixStream::connect(&sock_p)
        .await
        .context("Failed to connect to Maclet VM control socket")?;

    let (r, mut w) = stream.into_split();
    let mut buf_r = tokio::io::BufReader::new(r);

    let cnt_id = format!(
        "cnt-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis()
    );

    let req = HostMessage::RunContainer {
        id: cnt_id.clone(),
        name,
        image,
        command: cmd,
        args: final_args,
        env: HashMap::new(),
        hostname,
        limits: Some(limits),
        detached: detach,
        ports: publish,
        read_only,
        drop_caps: cap_drop,
        timeout_secs: 60,
    };

    write_async(&mut w, &req).await?;
    let resp: Option<GuestMessage> = read_with_timeout(&mut buf_r, 60).await?;

    match resp {
        Some(GuestMessage::ContainerStarted { id, .. }) => {
            println!("{}", id);
            Ok(0)
        }
        Some(GuestMessage::ContainerResult {
            id,
            exit_code,
            stdout,
            stderr,
            metrics,
        }) => {
            if !stdout.is_empty() {
                print!("{}", stdout);
            }
            if !stderr.is_empty() {
                eprint!("{}", stderr);
            }

            if show_stats {
                if let Some(m) = metrics {
                    println!(
                        "\n{}",
                        format!("=== Resource Usage: {} ===", id).bold().cyan()
                    );
                    let mem_used_mb = m.memory_current as f64 / (1024.0 * 1024.0);
                    let mem_max_str = m
                        .memory_max
                        .map(|b| format!("{:.1} MB", b as f64 / (1024.0 * 1024.0)))
                        .unwrap_or_else(|| "unlimited".to_string());
                    println!("Memory: {:.2} MB / {}", mem_used_mb, mem_max_str);

                    let cpu_sec = m.cpu_usage_usec as f64 / 1_000_000.0;
                    println!(
                        "CPU Usage: {:.3}s (user: {:.3}s, sys: {:.3}s)",
                        cpu_sec,
                        m.cpu_user_usec as f64 / 1_000_000.0,
                        m.cpu_system_usec as f64 / 1_000_000.0
                    );

                    let pids_max_str = m
                        .pids_max
                        .map(|p| p.to_string())
                        .unwrap_or_else(|| "unlimited".to_string());
                    println!("PIDs: {} / {}", m.pids_current, pids_max_str);
                }
            }

            Ok(exit_code)
        }
        Some(GuestMessage::Error { message, .. }) => {
            eprintln!("{}: {}", "Container execution error".bold().red(), message);
            Ok(1)
        }
        other => {
            eprintln!("Unexpected response from guest agent: {:?}", other);
            Ok(1)
        }
    }
}

async fn handle_ps(all: bool) -> Result<i32> {
    let sock_p = socket_path();
    if !sock_p.exists() {
        return Err(anyhow!("Maclet VM is not running. Start with 'maclet machine start'."));
    }

    let stream = UnixStream::connect(&sock_p).await?;
    let (r, mut w) = stream.into_split();
    let mut buf_r = tokio::io::BufReader::new(r);

    let req = HostMessage::ListContainers { all };
    write_async(&mut w, &req).await?;

    let resp: Option<GuestMessage> = read_with_timeout(&mut buf_r, 10).await?;
    match resp {
        Some(GuestMessage::ContainerList { containers }) => {
            println!(
                "{:<14} {:<16} {:<20} {:<14} {:<14} {:<16} {:<14}",
                "CONTAINER ID", "IMAGE", "COMMAND", "CREATED", "STATUS", "PORTS", "NAMES"
            );
            for c in containers {
                let id_short = if c.id.len() > 12 { &c.id[..12] } else { &c.id };
                let cmd_short = if c.command.len() > 18 {
                    format!("\"{}...\"", &c.command[..15])
                } else {
                    format!("\"{}\"", c.command)
                };
                let status_str = match c.status {
                    maclet_common::ContainerStatus::Running => "Up".green().to_string(),
                    maclet_common::ContainerStatus::Exited => {
                        let code = c.exit_code.unwrap_or(0);
                        if code == 0 {
                            format!("Exited ({})", code).cyan().to_string()
                        } else {
                            format!("Exited ({})", code).red().to_string()
                        }
                    }
                    maclet_common::ContainerStatus::Stopped => "Stopped".yellow().to_string(),
                    maclet_common::ContainerStatus::Created => "Created".normal().to_string(),
                };
                let rel_time = format_relative_time(c.created_at);
                let ports_str = if !c.ports.is_empty() {
                    c.ports.join(", ")
                } else if let Some(ref ip) = c.ip {
                    ip.clone()
                } else {
                    "-".to_string()
                };
                println!(
                    "{:<14} {:<16} {:<20} {:<14} {:<14} {:<16} {:<14}",
                    id_short, c.image, cmd_short, rel_time, status_str, ports_str, c.name
                );
            }
            Ok(0)
        }
        Some(GuestMessage::Error { message, .. }) => {
            eprintln!("{}: {}", "Error".bold().red(), message);
            Ok(1)
        }
        _ => {
            eprintln!("Failed to get container list");
            Ok(1)
        }
    }
}

async fn handle_stop_container(id: String, time: u64) -> Result<i32> {
    let sock_p = socket_path();
    if !sock_p.exists() {
        return Err(anyhow!("Maclet VM is not running."));
    }

    let stream = UnixStream::connect(&sock_p).await?;
    let (r, mut w) = stream.into_split();
    let mut buf_r = tokio::io::BufReader::new(r);

    let req = HostMessage::StopContainer {
        id: id.clone(),
        timeout_secs: time,
    };
    write_async(&mut w, &req).await?;

    let resp: Option<GuestMessage> = read_with_timeout(&mut buf_r, time + 5).await?;
    match resp {
        Some(GuestMessage::ContainerStopped { id, .. }) => {
            println!("{}", id);
            Ok(0)
        }
        Some(GuestMessage::Error { message, .. }) => {
            eprintln!("{}: {}", "Stop error".bold().red(), message);
            Ok(1)
        }
        _ => {
            eprintln!("Failed to stop container");
            Ok(1)
        }
    }
}

async fn handle_rm(id: String, force: bool) -> Result<i32> {
    let sock_p = socket_path();
    if !sock_p.exists() {
        return Err(anyhow!("Maclet VM is not running."));
    }

    let stream = UnixStream::connect(&sock_p).await?;
    let (r, mut w) = stream.into_split();
    let mut buf_r = tokio::io::BufReader::new(r);

    let req = HostMessage::RemoveContainer {
        id: id.clone(),
        force,
    };
    write_async(&mut w, &req).await?;

    let resp: Option<GuestMessage> = read_with_timeout(&mut buf_r, 10).await?;
    match resp {
        Some(GuestMessage::ContainerRemoved { id }) => {
            println!("{}", id);
            Ok(0)
        }
        Some(GuestMessage::Error { message, .. }) => {
            eprintln!("{}: {}", "Remove error".bold().red(), message);
            Ok(1)
        }
        _ => {
            eprintln!("Failed to remove container");
            Ok(1)
        }
    }
}

async fn handle_logs(id: String, tail: Option<usize>) -> Result<i32> {
    let sock_p = socket_path();
    if !sock_p.exists() {
        return Err(anyhow!("Maclet VM is not running."));
    }

    let stream = UnixStream::connect(&sock_p).await?;
    let (r, mut w) = stream.into_split();
    let mut buf_r = tokio::io::BufReader::new(r);

    let req = HostMessage::ContainerLogs {
        id: id.clone(),
        tail,
    };
    write_async(&mut w, &req).await?;

    let resp: Option<GuestMessage> = read_with_timeout(&mut buf_r, 10).await?;
    match resp {
        Some(GuestMessage::ContainerLogsResult { logs, .. }) => {
            print!("{}", logs);
            Ok(0)
        }
        Some(GuestMessage::Error { message, .. }) => {
            eprintln!("{}: {}", "Logs error".bold().red(), message);
            Ok(1)
        }
        _ => {
            eprintln!("Failed to retrieve logs");
            Ok(1)
        }
    }
}

async fn handle_inspect(id: String) -> Result<i32> {
    let sock_p = socket_path();
    if !sock_p.exists() {
        return Err(anyhow!("Maclet VM is not running."));
    }

    let stream = UnixStream::connect(&sock_p).await?;
    let (r, mut w) = stream.into_split();
    let mut buf_r = tokio::io::BufReader::new(r);

    let req = HostMessage::InspectContainer { id: id.clone() };
    write_async(&mut w, &req).await?;

    let resp: Option<GuestMessage> = read_with_timeout(&mut buf_r, 10).await?;
    match resp {
        Some(GuestMessage::InspectResult { details, .. }) => {
            println!("{}", serde_json::to_string_pretty(&details)?);
            Ok(0)
        }
        Some(GuestMessage::Error { message, .. }) => {
            eprintln!("{}: {}", "Inspect error".bold().red(), message);
            Ok(1)
        }
        _ => {
            eprintln!("No inspection data received");
            Ok(1)
        }
    }
}

async fn handle_stats(id: Option<String>) -> Result<i32> {
    let sock_p = socket_path();
    if !sock_p.exists() {
        return Err(anyhow!("Maclet VM is not running."));
    }

    let stream = UnixStream::connect(&sock_p).await?;
    let (r, mut w) = stream.into_split();
    let mut buf_r = tokio::io::BufReader::new(r);

    if let Some(target_id) = id {
        let req = HostMessage::InspectContainer {
            id: target_id.clone(),
        };
        write_async(&mut w, &req).await?;
        let resp: Option<GuestMessage> = read_with_timeout(&mut buf_r, 10).await?;
        match resp {
            Some(GuestMessage::InspectResult { details, .. }) => {
                println!(
                    "{:<16} {:<16} {:<12} {:<24} {:<10}",
                    "CONTAINER ID", "NAME", "CPU %", "MEM USAGE / LIMIT", "PIDS"
                );
                let id_short = if details.id.len() > 12 {
                    &details.id[..12]
                } else {
                    &details.id
                };
                let (mem_str, pids_str) = if let Some(m) = details.metrics {
                    let mem_used_mb = m.memory_current as f64 / (1024.0 * 1024.0);
                    let mem_max_str = m
                        .memory_max
                        .map(|b| format!("{:.1}MB", b as f64 / (1024.0 * 1024.0)))
                        .unwrap_or_else(|| "unlimited".to_string());
                    (
                        format!("{:.2}MB / {}", mem_used_mb, mem_max_str),
                        m.pids_current.to_string(),
                    )
                } else {
                    ("-- / --".to_string(), "--".to_string())
                };
                println!(
                    "{:<16} {:<16} {:<12} {:<24} {:<10}",
                    id_short, details.name, "0.00%", mem_str, pids_str
                );
                Ok(0)
            }
            Some(GuestMessage::Error { message, .. }) => {
                eprintln!("{}: {}", "Stats error".bold().red(), message);
                Ok(1)
            }
            _ => Ok(1),
        }
    } else {
        let req = HostMessage::ListContainers { all: false };
        write_async(&mut w, &req).await?;
        let resp: Option<GuestMessage> = read_with_timeout(&mut buf_r, 10).await?;
        match resp {
            Some(GuestMessage::ContainerList { containers }) => {
                println!(
                    "{:<16} {:<16} {:<12} {:<24} {:<10}",
                    "CONTAINER ID", "NAME", "CPU %", "MEM USAGE / LIMIT", "PIDS"
                );
                for c in containers {
                    let id_short = if c.id.len() > 12 { &c.id[..12] } else { &c.id };
                    let mem_used = c.memory_current.unwrap_or(0) as f64 / (1024.0 * 1024.0);
                    let mem_max_str = c
                        .memory_max
                        .map(|b| format!("{:.1}MB", b as f64 / (1024.0 * 1024.0)))
                        .unwrap_or_else(|| "unlimited".to_string());
                    let mem_str = format!("{:.2}MB / {}", mem_used, mem_max_str);
                    println!(
                        "{:<16} {:<16} {:<12} {:<24} {:<10}",
                        id_short,
                        c.name,
                        "0.00%",
                        mem_str,
                        c.pid.map(|_| "1").unwrap_or("0")
                    );
                }
                Ok(0)
            }
            _ => Ok(1),
        }
    }
}

fn handle_version() {
    println!("{} {}", "maclet".bold(), env!("CARGO_PKG_VERSION"));
    println!("Architecture: {}", std::env::consts::ARCH);
    println!("OS: {}", std::env::consts::OS);
    println!(
        "Apple Virtualization Framework Supported: {}",
        if VirtualMachine::is_supported() {
            "Yes".green()
        } else {
            "No".red()
        }
    );
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();

    let res = match cli.command {
        Commands::Machine { action } => match action {
            MachineCommands::Init { cpus, memory } => handle_init(cpus, memory).await.map(|_| 0),
            MachineCommands::Start { daemon } => handle_start(daemon).await.map(|_| 0),
            MachineCommands::Stop => handle_machine_stop().await.map(|_| 0),
            MachineCommands::Status => handle_status().await.map(|_| 0),
        },
        Commands::Exec {
            target,
            command,
            args,
        } => handle_exec(target, command, args).await,
        Commands::Pull { image } => handle_pull(image).await,
        Commands::Images => handle_images().await,
        Commands::Run {
            detach,
            name,
            image,
            command,
            args,
            hostname,
            memory,
            cpus,
            pids_limit,
            publish,
            read_only,
            cap_drop,
            stats,
        } => {
            handle_run(
                detach, name, image, command, args, hostname, memory, cpus, pids_limit, publish,
                read_only, cap_drop, stats,
            )
            .await
        }
        Commands::Ps { all } => handle_ps(all).await,
        Commands::Stop { id, time } => handle_stop_container(id, time).await,
        Commands::Rm { id, force } => handle_rm(id, force).await,
        Commands::Logs { id, tail } => handle_logs(id, tail).await,
        Commands::Inspect { id } => handle_inspect(id).await,
        Commands::Stats { id } => handle_stats(id).await,
        Commands::System { command } => match command {
            SystemCommands::Info => handle_system_info().await,
        },
        Commands::Version => {
            handle_version();
            Ok(0)
        }
    };

    match res {
        Ok(code) => ExitCode::from(code as u8),
        Err(e) => {
            eprintln!("{}: {:#}", "Error".bold().red(), e);
            ExitCode::FAILURE
        }
    }
}
