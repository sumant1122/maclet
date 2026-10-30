# Apple Virtualization Framework Deep Dive

## 1. Overview

Apple Virtualization Framework (`Virtualization.framework`) was introduced in macOS 11 (Big Sur) and substantially expanded in macOS 12 and 13. It runs in user space above `Hypervisor.framework`, providing high-level abstractions for managing virtual machines, virtual CPUs, memory, and standard virtio devices.

## 2. Linux VM Boot Process

Booting Linux via Apple VZ uses `VZLinuxBootLoader`:
```swift
let bootLoader = VZLinuxBootLoader(kernelURL: kernelURL)
bootLoader.initialRamdiskURL = initrdURL // Optional
bootLoader.commandLine = "console=hvc0 rdinit=/init quiet"
```

### Key Parameters:
- `kernelURL`: Path to ARM64 Linux `Image`. Must be a flat kernel binary (not a zboot wrapped wrapper).
- `initialRamdiskURL`: Gzip-compressed or uncompressed `cpio` archive.
- `commandLine`: Kernel parameters. `console=hvc0` directs output to the first Virtio console port. `rdinit=/init` points the kernel to our custom Rust init executable.

## 3. Virtual Hardware Configuration

```swift
let config = VZVirtualMachineConfiguration()
config.bootLoader = bootLoader
config.cpuCount = clamp(cpus, VZVirtualMachineConfiguration.minimumAllowedCPUCount, VZVirtualMachineConfiguration.maximumAllowedCPUCount)
config.memorySize = memoryBytes
```

### Device Support:
- **Serial Ports (`config.serialPorts`)**: `VZVirtioConsoleDeviceSerialPortConfiguration` allows adding Virtio console ports mapped to `/dev/hvc0`, `/dev/hvc1`, etc.
  - Attachment: `VZFileHandleSerialPortAttachment` allows binding any read/write file handle or socket descriptor.
- **Block Devices (`config.storageDevices`)**: `VZVirtioBlockDeviceConfiguration` attaches raw or qcow2-style disk images via `VZDiskImageStorageDeviceAttachment`.
- **Directory Sharing (`config.directorySharingDevices`)**: `VZVirtioFileSystemDeviceConfiguration` provides VirtioFS directory sharing with caching options (`VZVirtioFileSystemDeviceCacheMode`).
- **Networking (`config.networkDevices`)**: `VZVirtioNetworkDeviceConfiguration` supports NAT attachments (`VZNATNetworkDeviceAttachment`) or bridged attachments (`VZBridgedNetworkDeviceAttachment`).
- **Entropy (`config.entropyDevices`)**: `VZVirtioEntropyDeviceConfiguration` passes host randomness to the guest kernel for fast `getrandom()` initialization without blocking.

## 4. Lifecycle Management and Threading Model

- `VZVirtualMachine` must be created and invoked on a dedicated Dispatch Queue or run loop.
- Calling `vm.start(completionHandler:)` boots the virtual CPUs asynchronously.
- The delegate `VZVirtualMachineDelegate` notifies the host of lifecycle events:
  - `guestDidStop(_:)`: Triggered when Linux halts or powers off (`poweroff` / `reboot`).
  - `virtualMachine(_:didStopWithError:)`: Triggered on hypervisor or device emulation failure.
- Graceful shutdown: `vm.requestStop(completionHandler:)` issues an ACPI power button event to the guest, or Maclet's agent intercepts a shutdown command and performs `sync && reboot(RB_POWER_OFF)`.
