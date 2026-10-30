# Maclet Implementation Roadmap

## Phase 1: Linux VM Boot & Control Channel (Current Target)
- [x] Initial design documents and technical constraint verification.
- [ ] Root Rust workspace and base crates (`maclet-common`, `maclet-ipc`, `maclet-vm`, `maclet-guest-agent`, `maclet-cli`).
- [ ] Swift C-compatible FFI wrapper (`virtualization/apple-vz`) for Apple Virtualization Framework.
- [ ] Statically linked Linux guest agent (`maclet-guest-agent`) compiled for `aarch64-unknown-linux-musl`.
- [ ] VM boot tooling: `maclet machine init` to prepare Linux kernel and build initramfs with guest agent.
- [ ] Bidirectional IPC over Virtio Console socketpair.
- [ ] CLI command `maclet exec <cmd>` to run a process inside the booted VM.
- [ ] Unit and integration test suite.

## Phase 2: First Container Isolation
- Implement `maclet-namespaces` (`unshare`/`clone3`, mount points, `pivot_root`).
- Mount `/proc`, `/sys`, `/dev` in isolated mount namespace.
- Isolate UTS (hostname) and IPC.
- Run `/bin/sh` or custom command in isolated namespace.

## Phase 3: Resource Isolation (cgroups v2)
- Implement `maclet-cgroups` to manage `/sys/fs/cgroup/maclet/<id>/`.
- Apply CPU limits (`cpu.max`).
- Apply memory limits (`memory.max`).
- Process limits (`pids.max`).
- Query live resource statistics.

## Phase 4: Container Lifecycle & State Management
- Support concurrent containers with UUIDs.
- State persistence (`~/.maclet/containers/`).
- `maclet ps`, `maclet stop`, `maclet rm`, `maclet logs`.

## Phase 5: OCI Image Management & Storage
- Parse OCI Image Manifests and Index.
- Download layer blobs with SHA256 verification.
- Layer unpacking and cache management in `maclet-storage`.
- OverlayFS mount management (`lowerdir`, `upperdir`, `workdir`).

## Phase 6: Networking
- Bridge creation (`maclet0`) inside Linux guest.
- veth pair creation and netns assignment.
- IPAM (IP Address Management) in 10.88.0.0/16.
- NAT / iptables masquerade for outbound internet.
- Host-to-guest port forwarding via TCP proxy.

## Phase 7: Security Hardening
- Seccomp profile compilation (BPF filter).
- Linux capability bounding set reduction.
- User namespace UID/GID mappings.

## Phase 8: Production Polish
- Daemon autostart, crash recovery.
- Diagnostic logs and benchmarking.
