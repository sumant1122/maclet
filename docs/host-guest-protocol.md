# Host/Guest Communication Protocol Specification

## 1. Framing and Transport

All messages between the macOS host (`maclet-vm` / `maclet-daemon`) and the Linux guest (`maclet-guest-agent`) are transmitted over the dedicated Virtio channel (`/dev/hvc1`) using a framed JSON protocol.

### Wire Format
Each message is serialized as a JSON object terminated by a newline (`\n` / `0x0A`):
```
+-----------------------------------------------------------+------+
|                    JSON Payload (UTF-8)                   | 0x0A |
+-----------------------------------------------------------+------+
```

JSON framing provides:
- Human-inspectable protocol streams during debugging.
- Resilient recovery from framing anomalies.
- Natural alignment with Rust `serde_json` and streaming deserializers.

## 2. Message Types

### 2.1 Initialization & Handshake
Upon boot, the guest agent connects to the channel and issues a `Ready` event:
```json
{
  "type": "ready",
  "version": "0.1.0",
  "kernel": "6.6.134-0-virt",
  "arch": "aarch64",
  "uptime_secs": 1
}
```

The host confirms connection with `Ping`:
```json
{
  "id": "req-1",
  "type": "ping"
}
```

Guest replies with `Pong`:
```json
{
  "id": "req-1",
  "type": "pong",
  "timestamp": 1730304000
}
```

### 2.2 Command Execution (`Exec`)
Used to run commands directly inside the guest VM:

**Request (Host -> Guest):**
```json
{
  "id": "req-2",
  "type": "exec",
  "command": "uname",
  "args": ["-a"],
  "env": {
    "PATH": "/usr/local/bin:/usr/bin:/bin"
  },
  "timeout_secs": 30
}
```

**Response (Guest -> Host):**
```json
{
  "id": "req-2",
  "type": "exec_result",
  "exit_code": 0,
  "stdout": "Linux maclet-guest 6.6.134-0-virt #1-Alpine SMP PREEMPT_DYNAMIC aarch64 Linux\n",
  "stderr": ""
}
```

### 2.3 Container Lifecycle Messages (Phases 2-6)
Future phases expand this protocol for container creation:
- `create_container`: Defines namespaces, mounts, cgroup limits, rootfs path.
- `start_container`: Launches container init process.
- `exec_container`: Runs a process inside an existing container namespace.
- `stop_container`: Sends `SIGTERM` / `SIGKILL`.
- `delete_container`: Unmounts OverlayFS, cleans up cgroup and veth.

### 2.4 System Control
- `shutdown`: Instructs `maclet-guest-agent` to flush filesystems and perform graceful system halt.
```json
{
  "id": "req-99",
  "type": "shutdown"
}
```
