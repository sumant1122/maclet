# Maclet

**Maclet** is a native macOS container runtime written in Rust. It creates, runs, and isolates Linux containers without depending on Docker Desktop, Colima, or containerd.

Because macOS lacks native Linux kernel primitives (namespaces, cgroups, OverlayFS), Maclet implements a clean host/guest architecture:
- **macOS Host**: CLI, VM lifecycle orchestrator via Apple's Virtualization Framework, local IPC, and image cache management.
- **Linux Guest**: A real minimal Linux micro-kernel and the statically linked `maclet-guest-agent` which directly manages Linux namespaces, cgroups v2, OverlayFS, and process lifecycles.

---

## Architecture Overview

```
maclet CLI (macOS)
       │
       ▼ (Unix Domain Socket: ~/.maclet/maclet.sock)
maclet Daemon / maclet-vm
       │
       ▼ (Apple Virtualization Framework via Swift C-FFI)
Linux ARM64 Guest VM
       │
       ▼ (Virtio Console Channel: /dev/hvc1)
maclet-guest-agent (Rust static binary as PID 1)
       │
       ├─► Linux Namespaces (PID, Mount, Net, IPC, UTS, User)
       ├─► cgroups v2 Quotas (cpu.max, memory.max, pids.max)
       ├─► OverlayFS & pivot_root
       └─► Containerized Processes
```

---

## Project Structure

```text
maclet/
├── Cargo.toml
├── Makefile
├── README.md
├── LICENSE
├── entitlements.plist
│
├── docs/
│   ├── architecture.md           # System architecture & data flow
│   ├── technical-constraints.md  # Darwin limits & hypervisor workarounds
│   ├── apple-vz-research.md      # Apple Virtualization Framework internals
│   ├── kernel-configuration.md   # Minimum Linux kernel config
│   ├── host-guest-protocol.md    # Framed JSON wire protocol spec
│   ├── container-lifecycle.md    # Container state machine & transitions
│   ├── crate-boundaries.md       # Rust crate dependency structure
│   ├── security.md               # Hardware/kernel security model
│   ├── implementation-roadmap.md # Phased implementation roadmap
│   └── risk-analysis.md          # Architectural risks & mitigations
│
├── crates/
│   ├── maclet-common/            # Protocol models, errors & VM configs
│   ├── maclet-ipc/               # Async & sync newline-framed JSON stream
│   ├── maclet-vm/                # Apple Virtualization Framework Rust wrapper
│   ├── maclet-cli/               # CLI interface (clap) & daemon manager
│   └── maclet-guest-agent/       # Statically linked Linux init & agent
│
├── virtualization/
│   └── apple-vz/                 # Swift FFI wrapper around VZVirtualMachine
│       ├── include/apple_vz.h
│       └── Sources/apple_vz.swift
│
└── guest/                        # Kernel and initramfs tooling
```

---

## Quickstart (Phase 1: VM Boot & Execution)

### Prerequisites

1. macOS running on Apple Silicon (ARM64).
2. Xcode Command Line Tools (`xcode-select --install`).
3. Rust (`rustup default stable` and `rustup target add aarch64-unknown-linux-musl`).

### Build & Test

```bash
# Build release binary and sign with virtualization entitlement
make release

# Run all unit and integration tests (including live VM boot)
make test
```

### Usage

```bash
# 1. Initialize environment (prepares Linux kernel & builds static guest agent)
./target/release/maclet machine init

# 2. Start the Linux micro-VM in the background
./target/release/maclet machine start --daemon

# 3. Check VM and guest agent status
./target/release/maclet machine status

# 4. Execute commands directly inside the real Linux kernel
./target/release/maclet exec uname -a
./target/release/maclet exec cat /proc/version
./target/release/maclet exec cat /proc/cpuinfo
./target/release/maclet exec cat /proc/meminfo
./target/release/maclet exec uptime

# 5. Stop the virtual machine gracefully
./target/release/maclet machine stop
```

---

## Phase 1 Success Verification

Phase 1 achieves:
- Native Apple Virtualization Framework backend written in Swift and linked into Rust with zero runtime dependencies.
- Sub-second boot of a 64-bit Linux micro-kernel on Apple Silicon.
- Dedicated Virtio Console IPC channel over a full-duplex Unix socketpair.
- Minimal Rust guest agent running statically inside Linux.
- Live command execution inside the Linux VM driven from the macOS CLI.

See `docs/` for technical design specifications for Phase 2 (container isolation) and beyond.
