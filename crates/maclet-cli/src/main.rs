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

    /// Execute a command inside the Linux guest environment
    #[command(trailing_var_arg = true)]
    Exec {
        /// Command to execute
        command: String,

        /// Arguments for the command
        #[arg(allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Run an isolated Linux container with its own namespaces, rootfs, and pivot_root
    #[command(trailing_var_arg = true)]
    Run {
        /// Container command to execute
        #[arg(default_value = "/bin/sh")]
        command: String,

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

        /// Display cgroup v2 resource usage statistics upon completion
        #[arg(long)]
        stats: bool,
    },

    /// Inspect live resource metrics of a container
    Inspect {
        /// Container identifier
        id: String,
    },

    /// Display version and environment information
    Version,
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

    let rootfs_tar = initramfs::ensure_rootfs_tar(&kernel_dir)?;
    println!("Base rootfs ready at: {}", rootfs_tar.display().to_string().green());

    let workspace_root = find_workspace_root()?;
    let guest_bin = initramfs::build_guest_agent(&workspace_root)?;
    let initramfs_path = kernel_dir.join("initrd.img");
    initramfs::create_initramfs(&guest_bin, &rootfs_tar, &initramfs_path)?;

    let config = VmConfig {
        cpu_count: cpus,
        memory_mb: memory,
        kernel_path,
        initramfs_path,
        cmdline: "console=hvc0 rdinit=/init quiet".to_string(),
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

                        if is_shutdown {
                            std::process::exit(0);
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
    let resp: Option<GuestMessage> = read_async(&mut buf_r).await?;

    if let Some(GuestMessage::Pong { id }) = resp {
        if id == "status-check" {
            println!("{}", "Status: Maclet VM is RUNNING (guest agent responding)".bold().green());
            return Ok(());
        }
    }

    println!("{}", "Status: UNKNOWN (guest agent did not respond properly)".red());
    Ok(())
}

async fn handle_stop() -> Result<()> {
    let sock_p = socket_path();
    if !sock_p.exists() {
        println!("{}", "Maclet VM is already stopped.".yellow());
        return Ok(());
    }

    let stream = match UnixStream::connect(&sock_p).await {
        Ok(s) => s,
        Err(_) => {
            println!("{}", "Maclet VM is not running (cleaning up socket).".yellow());
            let _ = fs::remove_file(&sock_p);
            return Ok(());
        }
    };

    let (r, mut w) = stream.into_split();
    let mut buf_r = tokio::io::BufReader::new(r);

    println!("Sending shutdown command to guest VM...");
    write_async(&mut w, &HostMessage::Shutdown { id: "cli-stop".to_string() }).await?;
    let _ = read_async::<_, GuestMessage>(&mut buf_r).await;

    let _ = fs::remove_file(&sock_p);
    println!("{}", "Maclet Linux VM stopped successfully.".green());
    Ok(())
}

async fn handle_exec(command: String, args: Vec<String>) -> Result<i32> {
    let sock_p = socket_path();
    if !sock_p.exists() {
        return Err(anyhow!(
            "Maclet VM is not running. Start it with 'maclet machine start' first."
        ));
    }

    let stream = UnixStream::connect(&sock_p).await
        .context("Failed to connect to Maclet VM control socket")?;

    let (r, mut w) = stream.into_split();
    let mut buf_r = tokio::io::BufReader::new(r);

    let req_id = format!("exec-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_millis());
    let req = HostMessage::Exec {
        id: req_id.clone(),
        command,
        args,
        env: HashMap::new(),
        timeout_secs: 30,
    };

    write_async(&mut w, &req).await?;
    let resp: Option<GuestMessage> = read_async(&mut buf_r).await?;

    match resp {
        Some(GuestMessage::ExecResult { id: _, exit_code, stdout, stderr }) => {
            if !stdout.is_empty() {
                print!("{}", stdout);
            }
            if !stderr.is_empty() {
                eprint!("{}", stderr);
            }
            Ok(exit_code)
        }
        Some(GuestMessage::Error { message, .. }) => {
            eprintln!("Guest agent returned error: {}", message);
            Ok(1)
        }
        other => {
            eprintln!("Unexpected response from guest: {:?}", other);
            Ok(1)
        }
    }
}

async fn handle_run(
    command: String,
    args: Vec<String>,
    hostname: Option<String>,
    memory: Option<String>,
    cpus: Option<f64>,
    pids_limit: Option<u64>,
    show_stats: bool,
) -> Result<i32> {
    let sock_p = socket_path();
    if !sock_p.exists() {
        return Err(anyhow!(
            "Maclet VM is not running. Start it with 'maclet machine start' first."
        ));
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
        image: "alpine".to_string(),
        command,
        args,
        env: HashMap::new(),
        hostname,
        limits: Some(limits),
        timeout_secs: 60,
    };

    write_async(&mut w, &req).await?;
    let resp: Option<GuestMessage> = read_async(&mut buf_r).await?;

    match resp {
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
            eprintln!("Container execution error: {}", message);
            Ok(1)
        }
        other => {
            eprintln!("Unexpected response from guest agent: {:?}", other);
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

    let resp: Option<GuestMessage> = read_async(&mut buf_r).await?;
    match resp {
        Some(GuestMessage::InspectResult { id, state, metrics }) => {
            println!(
                "{}",
                format!("=== Container Inspection: {} ===", id).bold().cyan()
            );
            println!("State: {}", state.green());
            println!(
                "Memory: {:.2} MB",
                metrics.memory_current as f64 / (1024.0 * 1024.0)
            );
            println!(
                "CPU Usage: {:.3}s",
                metrics.cpu_usage_usec as f64 / 1_000_000.0
            );
            println!("PIDs: {}", metrics.pids_current);
            Ok(0)
        }
        Some(GuestMessage::Error { message, .. }) => {
            eprintln!("Inspect error: {}", message);
            Ok(1)
        }
        _ => {
            eprintln!("No inspection data received");
            Ok(1)
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
            MachineCommands::Stop => handle_stop().await.map(|_| 0),
            MachineCommands::Status => handle_status().await.map(|_| 0),
        },
        Commands::Exec { command, args } => handle_exec(command, args).await,
        Commands::Run {
            command,
            args,
            hostname,
            memory,
            cpus,
            pids_limit,
            stats,
        } => handle_run(command, args, hostname, memory, cpus, pids_limit, stats).await,
        Commands::Inspect { id } => handle_inspect(id).await,
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
