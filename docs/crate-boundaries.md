# Rust Workspace Crate Boundaries

```
crates/
├── maclet-common/       # Domain models, errors, protocol messages (no heavy deps)
├── maclet-ipc/          # Framing, socket transport, async byte streams (Tokio)
├── maclet-vm/           # macOS Virtualization.framework bridge & VM manager (Host only)
├── maclet-daemon/       # Host background daemon managing VMs & state (Host only)
├── maclet-cli/          # Command-line interface with clap (Host only)
├── maclet-guest-agent/  # Minimal static init & container supervisor (Linux only)
├── maclet-namespaces/   # Linux clone/unshare/pivot_root wrappers (Linux only)
├── maclet-cgroups/      # Linux cgroups v2 controller (Linux only)
├── maclet-storage/      # OverlayFS & local rootfs manager (Linux only)
├── maclet-network/      # Bridge, veth, netns, and NAT (Linux only)
├── maclet-image/        # OCI registry client, layer unpacker, manifest parser
└── maclet-container/    # High-level container orchestrator
```

## Dependency Graph

```
maclet-cli ------------> maclet-common
     |
     +-(Unix Socket)--> maclet-daemon
                             |
                             +---> maclet-vm (Apple VZ)
                             |          | (Virtio Console)
                             |          v
                             |    maclet-guest-agent
                             |          |
                             |          +---> maclet-namespaces
                             |          +---> maclet-cgroups
                             |          +---> maclet-storage
                             |          +---> maclet-network
                             |
                             +---> maclet-image (OCI layers)
```

### Boundary Isolation Principles:
- `maclet-common` has zero platform-specific dependencies (compiles identically on Darwin and Linux).
- `maclet-vm` compiles strictly for `target_os = "macos"`.
- `maclet-guest-agent` and isolation crates compile statically for `target_os = "linux"` (`aarch64-unknown-linux-musl`).
- Host never imports Linux kernel headers; Guest never imports macOS Foundation/Virtualization frameworks.
