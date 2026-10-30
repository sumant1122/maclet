# Maclet Virtual Machine Management

## 1. Overview

Because macOS does not natively implement Linux namespaces, cgroups, or OverlayFS, Maclet boots a minimal, high-performance Linux virtual machine using Apple's **Virtualization Framework** (`Virtualization.framework`) on macOS.

---

## 2. Virtualization Architecture

```
+----------------------------------------------------------------+
|                          maclet-vm                             |
|       (Rust abstraction layer & configuration manager)         |
+----------------------------------------------------------------+
                               |
                        C FFI Boundary
                               |
                               v
+----------------------------------------------------------------+
|                     apple-vz (Swift Package)                   |
|                                                                |
|   VZVirtualMachine                                             |
|   VZVirtualMachineConfiguration                                |
|   VZLinuxBootLoader (kernel + initrd + cmdline)                |
|   VZVirtioConsoleDeviceConfiguration (control IPC)             |
|   VZVirtioBlockDeviceConfiguration (kernel modules modloop)    |
|   VZVirtioNetworkDeviceConfiguration (NAT attachment)          |
+----------------------------------------------------------------+
                               |
                               v
                    Apple Hypervisor.framework
```

---

## 3. Storage and Device Attachments

### 3.1 Kernel & Initramfs Boot
- **Kernel**: Alpine Linux `vmlinuz-virt` ARM64 kernel booted directly via `VZLinuxBootLoader`.
- **Initramfs**: Generated dynamically by `maclet machine init` using a custom cpio archive containing:
  - `/init`: The statically compiled Rust `maclet-guest-agent`.
  - `/rootfs.tar.gz`: The Alpine minirootfs archive.
  - Core boot drivers: `overlay.ko`, `squashfs.ko`, `loop.ko`, `virtio_blk.ko`, `virtio_net.ko`, `af_packet.ko`.

### 3.2 Block Device Attachment (`modloop.squashfs`)
- The full catalog of 1,158 Linux kernel modules is attached as a read-only virtio block device (`/dev/vda`) using `VZDiskImageStorageDeviceAttachment`.
- On boot, the guest agent mounts `/dev/vda` at `/.modloop` and symlinks `/lib/modules/<version>` directly to `/.modloop/modules/<version>`, enabling on-demand `modprobe` for any Linux driver without bloating initramfs size.

### 3.3 Virtio Console Control Channel
- A full-duplex Unix socketpair is connected to a `VZVirtioConsolePortConfiguration` (`/dev/hvc0`).
- Serves as the primary out-of-band IPC transport between `maclet-cli` and `maclet-guest-agent`.

---

## 4. CLI Machine Commands

```bash
# Initialize Linux kernel, modules, and guest agent
maclet machine init --cpus 2 --memory 1024

# Start the Maclet Linux VM daemon in background
maclet machine start --daemon

# Check status of the VM and guest agent
maclet machine status

# Gracefully stop the VM
maclet machine stop
```
