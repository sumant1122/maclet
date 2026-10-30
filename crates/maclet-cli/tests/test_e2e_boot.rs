use maclet_common::{GuestMessage, HostMessage, VmConfig};
use maclet_ipc::{read_async, write_async};
use maclet_vm::VirtualMachine;
use std::collections::HashMap;
use std::path::PathBuf;

fn dirs_home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
}

#[tokio::test]
async fn test_vm_boot_and_handshake_and_exec() {
    let kernel_path = dirs_home().join(".maclet/kernel/Image");
    let initramfs_path = dirs_home().join(".maclet/kernel/initrd.img");

    if !kernel_path.exists() || !initramfs_path.exists() {
        eprintln!("Kernel or initramfs not initialized, skipping e2e boot test");
        return;
    }

    let config = VmConfig {
        cpu_count: 2,
        memory_mb: 512,
        kernel_path,
        initramfs_path,
        cmdline: "console=hvc0 rdinit=/init quiet".to_string(),
    };

    let (mut vm, guest_channel) = VirtualMachine::start(&config, None)
        .expect("Failed to boot virtual machine");

    let (guest_reader, mut guest_writer) = guest_channel.into_split();
    let mut guest_buf_reader = tokio::io::BufReader::new(guest_reader);

    // Expect Ready
    let ready: Option<GuestMessage> = read_async(&mut guest_buf_reader).await.unwrap();
    assert!(matches!(ready, Some(GuestMessage::Ready { .. })));

    // Ping / Pong
    write_async(&mut guest_writer, &HostMessage::Ping { id: "test-ping".to_string() })
        .await
        .unwrap();
    let pong: Option<GuestMessage> = read_async(&mut guest_buf_reader).await.unwrap();
    assert_eq!(pong, Some(GuestMessage::Pong { id: "test-ping".to_string() }));

    // Exec uname -a
    write_async(
        &mut guest_writer,
        &HostMessage::Exec {
            id: "e2e-exec".to_string(),
            command: "uname".to_string(),
            args: vec!["-a".to_string()],
            env: HashMap::new(),
            timeout_secs: 10,
        },
    )
    .await
    .unwrap();

    let exec_res: Option<GuestMessage> = read_async(&mut guest_buf_reader).await.unwrap();
    if let Some(GuestMessage::ExecResult { id, exit_code, stdout, stderr }) = exec_res {
        assert_eq!(id, "e2e-exec");
        assert_eq!(exit_code, 0);
        assert!(stdout.contains("Linux"));
        assert!(stderr.is_empty());
    } else {
        panic!("Expected ExecResult, got {:?}", exec_res);
    }

    // Phase 2: Run an isolated container
    write_async(
        &mut guest_writer,
        &HostMessage::RunContainer {
            id: "e2e-cnt".to_string(),
            name: None,
            image: "alpine".to_string(),
            command: "/bin/sh".to_string(),
            args: vec![
                "-c".to_string(),
                "echo PID: $$; hostname; cat /etc/os-release".to_string(),
            ],
            env: HashMap::new(),
            hostname: Some("test-e2e-box".to_string()),
            limits: None,
            detached: false,
            timeout_secs: 20,
        },
    )
    .await
    .unwrap();

    let cnt_res: Option<GuestMessage> = read_async(&mut guest_buf_reader).await.unwrap();
    if let Some(GuestMessage::ContainerResult { id, exit_code, stdout, stderr, metrics }) = cnt_res {
        assert_eq!(id, "e2e-cnt");
        assert_eq!(exit_code, 0, "Container failed with stderr: {}", stderr);
        assert!(stdout.contains("PID: 1"), "PID was not 1: {}", stdout);
        assert!(stdout.contains("test-e2e-box"), "Hostname mismatch: {}", stdout);
        assert!(stdout.contains("Alpine Linux"), "Missing OS release: {}", stdout);
        assert!(metrics.is_some());
    } else {
        panic!("Expected ContainerResult, got {:?}", cnt_res);
    }

    // Phase 3: Run container with cgroups v2 resource limits
    let mem_limit = 256 * 1024 * 1024;
    write_async(
        &mut guest_writer,
        &HostMessage::RunContainer {
            id: "e2e-cgroup".to_string(),
            name: None,
            image: "alpine".to_string(),
            command: "/bin/sh".to_string(),
            args: vec![
                "-c".to_string(),
                "cat /sys/fs/cgroup/memory.max; cat /sys/fs/cgroup/pids.max".to_string(),
            ],
            env: HashMap::new(),
            hostname: Some("cgroup-test".to_string()),
            limits: Some(
                maclet_common::ResourceLimits::new()
                    .with_memory(mem_limit)
                    .with_cpus(1.0)
                    .with_pids(50),
            ),
            detached: false,
            timeout_secs: 20,
        },
    )
    .await
    .unwrap();

    let cg_res: Option<GuestMessage> = read_async(&mut guest_buf_reader).await.unwrap();
    if let Some(GuestMessage::ContainerResult { id, exit_code, stdout: _, stderr, metrics }) = cg_res {
        assert_eq!(id, "e2e-cgroup");
        assert_eq!(exit_code, 0, "Cgroup container failed: {}", stderr);
        let m = metrics.expect("Expected metrics");
        assert_eq!(m.memory_max, Some(mem_limit));
        assert_eq!(m.pids_max, Some(50));
        assert!(m.cpu_usage_usec > 0);
    } else {
        panic!("Expected ContainerResult, got {:?}", cg_res);
    }

    // Phase 4: Lifecycle - Detached run, exec inside container, inspect, stop, and remove
    write_async(
        &mut guest_writer,
        &HostMessage::RunContainer {
            id: "e2e-detached".to_string(),
            name: Some("test-worker".to_string()),
            image: "alpine".to_string(),
            command: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), "sleep 60".to_string()],
            env: HashMap::new(),
            hostname: Some("worker-host".to_string()),
            limits: None,
            detached: true,
            timeout_secs: 10,
        },
    )
    .await
    .unwrap();

    let start_res: Option<GuestMessage> = read_async(&mut guest_buf_reader).await.unwrap();
    match start_res {
        Some(GuestMessage::ContainerStarted { id, name, .. }) => {
            assert_eq!(id, "e2e-detached");
            assert_eq!(name, "test-worker");
        }
        other => panic!("Expected ContainerStarted, got {:?}", other),
    }

    // Exec inside running container
    write_async(
        &mut guest_writer,
        &HostMessage::ExecInContainer {
            id: "e2e-detached".to_string(),
            command: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), "echo exec-e2e-ok".to_string()],
            env: HashMap::new(),
        },
    )
    .await
    .unwrap();

    let exec_res: Option<GuestMessage> = read_async(&mut guest_buf_reader).await.unwrap();
    match exec_res {
        Some(GuestMessage::ContainerExecResult { exit_code, stdout, .. }) => {
            assert_eq!(exit_code, 0);
            assert!(stdout.contains("exec-e2e-ok"));
        }
        other => panic!("Expected ContainerExecResult, got {:?}", other),
    }

    // Stop container
    write_async(
        &mut guest_writer,
        &HostMessage::StopContainer {
            id: "e2e-detached".to_string(),
            timeout_secs: 2,
        },
    )
    .await
    .unwrap();

    let stop_res: Option<GuestMessage> = read_async(&mut guest_buf_reader).await.unwrap();
    assert!(matches!(stop_res, Some(GuestMessage::ContainerStopped { id, .. }) if id == "e2e-detached"));

    // Remove container
    write_async(
        &mut guest_writer,
        &HostMessage::RemoveContainer {
            id: "e2e-detached".to_string(),
            force: false,
        },
    )
    .await
    .unwrap();

    let rm_res: Option<GuestMessage> = read_async(&mut guest_buf_reader).await.unwrap();
    assert!(matches!(rm_res, Some(GuestMessage::ContainerRemoved { id }) if id == "e2e-detached"));

    // Graceful stop
    vm.stop().expect("Failed to stop VM");
}
