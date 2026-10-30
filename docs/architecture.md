# Maclet Architecture Specification

## 1. Executive Summary

**Maclet** is a native macOS container runtime written in Rust that creates, runs, and isolates Linux containers without relying on Docker Desktop, Colima, or containerd. Because macOS does not have native Linux namespaces, cgroups, or OverlayFS, Maclet adopts a host/guest split architecture:

- **Host (macOS)**: CLI, management daemon, OCI image handling, storage layer caching, and Apple Virtualization Framework management.
- **Guest (Linux ARM64 / x86_64)**: A minimal Linux micro-kernel and the statically linked `maclet-guest-agent` which directly manipulates Linux namespaces, cgroups v2, OverlayFS, and process lifecycles.

```
+-------------------------------------------------------------------------+
|                              macOS Host                                 |
|                                                                         |
|   +-------------------+                     +-----------------------+   |
|   |    maclet-cli     | <--- Unix Socket --->|     maclet-daemon     |   |
|   +-------------------+                     +-----------------------+   |
|                                                         |               |
|                                             +-----------------------+   |
|                                             |       maclet-vm       |   |
|                                             +-----------------------+   |
|                                                         | (Swift FFI)   |
|                                             +-----------------------+   |
|                                             |    apple-vz (VZ VM)   |   |
+---------------------------------------------------------|---------------+
                                                          |
                      Virtio Console IPC (socketpair)     |
                      Virtio-Blk / Virtio-Net / VirtioFS  |
                                                          v
+-------------------------------------------------------------------------+
|                             Linux Guest VM                              |
|                                                                         |
|   +-----------------------------------------------------------------+   |
|   |                        Linux Kernel                             |   |
|   +-----------------------------------------------------------------+   |
|                                  |                                      |
|   +-----------------------------------------------------------------+   |
|   |                       maclet-guest-agent                        |   |
|   +-----------------------------------------------------------------+   |
|               |                                      |                  |
|     +-------------------+                  +-------------------+        |
|     |  Container A      |                  |  Container B      |        |
|     |  (PID, Mnt, Net,  |                  |  (PID, Mnt, Net,  |        |
|     |   Cgroup v2, etc) |                  |   Cgroup v2, etc) |        |
|     +-------------------+                  +-------------------+        |
+-------------------------------------------------------------------------+
```

## 2. Core Subsystems

### 2.1 macOS Host
1. **`maclet-cli`**: Command-line interface parsing commands via `clap`, communicating with `maclet-daemon` over a Unix domain socket (`~/.maclet/maclet.sock`).
2. **`maclet-daemon`**: Central control daemon. Coordinates VM lifecycles, container metadata store (JSON/SQLite), image registry client, and host port forwarders.
3. **`maclet-vm`**: Hypervisor abstraction crate. Talks to the native Swift/Objective-C `Virtualization.framework` through a C FFI layer (`apple-vz`). Manages memory, vCPUs, virtual devices, and VM state.
4. **`maclet-ipc`**: Message serialization, framing, and bidirectional transport. Uses length-delimited JSON messages over Unix domain sockets on the host and virtio channels between host and guest.

### 2.2 Linux Guest
1. **Minimal Linux Kernel**: ARM64 kernel with built-in virtio drivers (`virtio-pci`, `virtio-mmio`, `virtio-blk`, `virtio-net`, `virtio-console`), cgroups v2, user/PID/mount/network namespaces, and OverlayFS.
2. **`maclet-guest-agent`**: The root process (`/init` or system service) inside the Linux VM. Implements container lifecycle operations:
   - Creating cgroups v2 hierarchy under `/sys/fs/cgroup/maclet/`.
   - Setting up network namespaces and veth interfaces.
   - Constructing rootfs overlays (lowerdir, upperdir, workdir).
   - Performing `pivot_root` and unsharing namespaces.
   - Spawning and monitoring container init processes.

## 3. Host-Guest Communication Model

Host-to-guest transport uses a dedicated **Virtio Console** bidirectional channel attached to a full-duplex socketpair on macOS:
- Port 0 (`/dev/hvc0`): Linux kernel console and boot log output (routed to macOS log or terminal).
- Port 1 (`/dev/hvc1`): Dedicated RPC protocol channel between `maclet-vm` and `maclet-guest-agent`.

This provides:
- Zero dependency on external network configuration or guest IP addresses.
- Sub-millisecond latency.
- Guaranteed in-order stream delivery with backpressure.
- Clean separation from container payload traffic.
