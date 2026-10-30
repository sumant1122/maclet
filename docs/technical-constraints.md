# macOS Technical Constraints & Architectural Solutions

## 1. Absence of Linux Primitives on Darwin

macOS does not support:
- Linux namespaces (`CLONE_NEWNS`, `CLONE_NEWPID`, `CLONE_NEWNET`, etc.).
- Linux Control Groups (cgroups v1/v2).
- Linux OverlayFS.
- Linux `pivot_root`.
- Linux Seccomp (`SECCOMP_SET_MODE_FILTER`).

### Solution
Never simulate Linux kernel behavior using macOS BSD subsystems. All container isolation happens inside a real, lightweight Linux virtual machine running under Apple Virtualization Framework. The macOS host acts strictly as a control plane, storage manager, and client orchestrator.

## 2. Apple Virtualization Framework Entitlements

On macOS, running code with `Virtualization.framework` (`VZVirtualMachine`) has strict security requirements:
- The binary must be signed with the `com.apple.security.virtualization` entitlement.
- Unlike `Hypervisor.framework`, an ad-hoc signature (`codesign --sign - --entitlements entitlements.plist --force <bin>`) is fully permitted on Apple Silicon for developer and user machines without an Apple Developer Program paid team certificate.
- When compiling Rust or Swift binaries, signing must be performed as part of the build pipeline before invoking the binary.

## 3. Kernel Image Format on Apple Silicon ARM64

- `VZLinuxBootLoader` requires an uncompressed 64-bit ARM Linux kernel (`Image`) with standard EFI stub / PE header (starting with `MZ` and containing magic `ARM\x64` at offset 0x38).
- Distribution kernels packaged as self-extracting zboot binaries (such as Alpine netboot `vmlinuz-virt`) encapsulate the gzip-compressed `Image` starting at an internal offset (e.g. byte 51568).
- `maclet` automatically decompresses or validates kernel binaries during `maclet machine init` to ensure fast, direct booting without firmware overhead.

## 4. Host-to-Guest Inter-Process Communication

- Virtio-VSOCK (`AF_VSOCK`) is supported by Apple Virtualization (`VZVirtioSocketDevice`), but modular Linux kernels (like Alpine's default) often build `CONFIG_VIRTIO_VSOCK` as a loadable kernel module rather than built-in monolithic code.
- Conversely, Virtio Console (`VZVirtioConsoleDeviceSerialPortConfiguration`) is compiled into virtually all minimal Linux kernels (`CONFIG_HVC_CONSOLE=y` and `CONFIG_VIRTIO_CONSOLE=y`).
- Apple Virtualization allows attaching a `VZFileHandleSerialPortAttachment` connected to a Unix `socketpair`. This provides a bidirectional, full-duplex communication channel between host and guest agent without requiring network interfaces, IP addresses, or loadable modules.

## 5. Filesystem Performance and Storage Sharing

- File sharing between macOS and the Linux VM using standard 9P (Plan 9) has significant latency and locking overhead.
- Apple Virtualization provides `VZVirtioFileSystemDeviceConfiguration` (VirtioFS), which shares macOS directories with near-native performance using DAX (Direct Access) shared memory.
- For container storage, Maclet keeps container layers and read-write disks in a formatted virtual disk image (Virtio-Blk) or uses VirtioFS to mount image caches directly from macOS.
