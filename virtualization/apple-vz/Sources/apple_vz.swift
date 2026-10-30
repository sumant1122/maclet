import Foundation
import Virtualization

final class VzVmWrapper: NSObject, VZVirtualMachineDelegate {
    let vm: VZVirtualMachine
    let queue: DispatchQueue
    var isRunning: Bool = false
    
    init(vm: VZVirtualMachine, queue: DispatchQueue) {
        self.vm = vm
        self.queue = queue
        super.init()
        self.vm.delegate = self
    }
    
    func guestDidStop(_ virtualMachine: VZVirtualMachine) {
        self.isRunning = false
    }
    
    func virtualMachine(_ virtualMachine: VZVirtualMachine, didStopWithError error: Error) {
        self.isRunning = false
    }
}

private func writeError(_ message: String, to buffer: UnsafeMutablePointer<CChar>?, capacity: Int) {
    guard let buffer = buffer, capacity > 0 else { return }
    message.withCString { cstr in
        strncpy(buffer, cstr, capacity - 1)
        buffer[capacity - 1] = 0
    }
}

@_cdecl("vz_is_supported")
public func vz_is_supported() -> Int32 {
    return VZVirtualMachine.isSupported ? 1 : 0
}

@_cdecl("vz_create_vm")
public func vz_create_vm(
    _ configPtr: UnsafePointer<VzConfig>?,
    _ consoleWriteFd: Int32,
    _ ipcSocketFd: Int32,
    _ outVm: UnsafeMutablePointer<UnsafeMutableRawPointer?>?,
    _ errBuf: UnsafeMutablePointer<CChar>?,
    _ errLen: Int
) -> Int32 {
    guard let config = configPtr?.pointee, let outVm = outVm else {
        writeError("Null configuration or output pointer", to: errBuf, capacity: errLen)
        return -1
    }
    
    guard let kernelPathStr = config.kernel_path else {
        writeError("Missing kernel_path", to: errBuf, capacity: errLen)
        return -1
    }
    let kernelURL = URL(fileURLWithPath: String(cString: kernelPathStr))
    let bootLoader = VZLinuxBootLoader(kernelURL: kernelURL)
    
    if let initrdPathStr = config.initrd_path {
        let initrdURL = URL(fileURLWithPath: String(cString: initrdPathStr))
        bootLoader.initialRamdiskURL = initrdURL
    }
    
    if let cmdlineStr = config.cmdline {
        bootLoader.commandLine = String(cString: cmdlineStr)
    }
    
    let vmConfig = VZVirtualMachineConfiguration()
    vmConfig.bootLoader = bootLoader
    vmConfig.cpuCount = max(1, Int(config.cpu_count))
    vmConfig.memorySize = max(256 * 1024 * 1024, config.memory_bytes)
    
    var serialPorts: [VZVirtioConsoleDeviceSerialPortConfiguration] = []
    
    // Port 0: Guest console output (always /dev/hvc0)
    let consoleConfig = VZVirtioConsoleDeviceSerialPortConfiguration()
    let consoleHandle: FileHandle
    if consoleWriteFd >= 0 {
        consoleHandle = FileHandle(fileDescriptor: consoleWriteFd, closeOnDealloc: false)
    } else {
        consoleHandle = Pipe().fileHandleForWriting
    }
    consoleConfig.attachment = VZFileHandleSerialPortAttachment(
        fileHandleForReading: nil,
        fileHandleForWriting: consoleHandle
    )
    serialPorts.append(consoleConfig)
    
    // Port 1: IPC socketpair (always /dev/hvc1)
    if ipcSocketFd >= 0 {
        let ipcHandle = FileHandle(fileDescriptor: ipcSocketFd, closeOnDealloc: false)
        let ipcConfig = VZVirtioConsoleDeviceSerialPortConfiguration()
        ipcConfig.attachment = VZFileHandleSerialPortAttachment(
            fileHandleForReading: ipcHandle,
            fileHandleForWriting: ipcHandle
        )
        serialPorts.append(ipcConfig)
    }
    
    vmConfig.serialPorts = serialPorts
    vmConfig.entropyDevices = [VZVirtioEntropyDeviceConfiguration()]

    // NAT Networking (Phase 6)
    if config.enable_nat_network != 0 {
        let netConfig = VZVirtioNetworkDeviceConfiguration()
        netConfig.attachment = VZNATNetworkDeviceAttachment()
        vmConfig.networkDevices = [netConfig]
    }

    // Virtio Block Storage (Phase 5 & 6 kernel modules / disk)
    if let diskPathStr = config.disk_path {
        let diskURL = URL(fileURLWithPath: String(cString: diskPathStr))
        if FileManager.default.fileExists(atPath: diskURL.path) {
            do {
                let attachment = try VZDiskImageStorageDeviceAttachment(url: diskURL, readOnly: true)
                let blockConfig = VZVirtioBlockDeviceConfiguration(attachment: attachment)
                vmConfig.storageDevices = [blockConfig]
            } catch {
                writeError("Failed to attach disk: \(error.localizedDescription)", to: errBuf, capacity: errLen)
                return -1
            }
        }
    }
    
    do {
        try vmConfig.validate()
    } catch {
        writeError("Configuration validation failed: \(error.localizedDescription)", to: errBuf, capacity: errLen)
        return -2
    }
    
    let queue = DispatchQueue(label: "com.maclet.vm.queue", qos: .userInitiated)
    let vm = VZVirtualMachine(configuration: vmConfig, queue: queue)
    let wrapper = VzVmWrapper(vm: vm, queue: queue)
    outVm.pointee = Unmanaged.passRetained(wrapper).toOpaque()
    return 0
}

@_cdecl("vz_start_vm")
public func vz_start_vm(
    _ vmPtr: UnsafeMutableRawPointer?,
    _ errBuf: UnsafeMutablePointer<CChar>?,
    _ errLen: Int
) -> Int32 {
    guard let vmPtr = vmPtr else {
        writeError("Null VM pointer", to: errBuf, capacity: errLen)
        return -1
    }
    let wrapper = Unmanaged<VzVmWrapper>.fromOpaque(vmPtr).takeUnretainedValue()
    
    let semaphore = DispatchSemaphore(value: 0)
    var startError: Error? = nil
    
    wrapper.queue.async {
        wrapper.vm.start { result in
            switch result {
            case .success:
                wrapper.isRunning = true
            case .failure(let error):
                startError = error
            }
            semaphore.signal()
        }
    }
    
    let waitResult = semaphore.wait(timeout: .now() + 15.0)
    if waitResult == .timedOut {
        writeError("VM start timed out", to: errBuf, capacity: errLen)
        return -3
    }
    
    if let error = startError {
        writeError("VM start failed: \(error.localizedDescription)", to: errBuf, capacity: errLen)
        return -2
    }
    return 0
}

@_cdecl("vz_stop_vm")
public func vz_stop_vm(
    _ vmPtr: UnsafeMutableRawPointer?,
    _ errBuf: UnsafeMutablePointer<CChar>?,
    _ errLen: Int
) -> Int32 {
    guard let vmPtr = vmPtr else {
        writeError("Null VM pointer", to: errBuf, capacity: errLen)
        return -1
    }
    let wrapper = Unmanaged<VzVmWrapper>.fromOpaque(vmPtr).takeUnretainedValue()
    
    let semaphore = DispatchSemaphore(value: 0)
    var stopError: Error? = nil
    
    wrapper.queue.async {
        wrapper.vm.stop { error in
            if let error = error {
                stopError = error
            } else {
                wrapper.isRunning = false
            }
            semaphore.signal()
        }
    }
    
    _ = semaphore.wait(timeout: .now() + 10.0)
    
    if let error = stopError {
        writeError("VM stop failed: \(error.localizedDescription)", to: errBuf, capacity: errLen)
        return -2
    }
    return 0
}

@_cdecl("vz_destroy_vm")
public func vz_destroy_vm(_ vmPtr: UnsafeMutableRawPointer?) {
    guard let vmPtr = vmPtr else { return }
    Unmanaged<VzVmWrapper>.fromOpaque(vmPtr).release()
}
