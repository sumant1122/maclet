# Maclet Security Boundaries and Privilege Model

## 1. Two-Tier Privilege Architecture

Maclet enforces isolation at two distinct boundaries:

1. **Hypervisor Isolation (Hardware Level)**:
   - The Linux VM executes in ARM Exception Level 1 (EL1), while macOS executes the hypervisor in EL2 (hardware virtualization).
   - Even if a container escapes root inside Linux, it is strictly confined to the guest micro-VM.
   - Host filesystem access is isolated; containers have no direct access to Darwin kernel structures or host memory.

2. **Container Isolation (Linux Kernel Level)**:
   - Namespaces (`CLONE_NEWNS`, `CLONE_NEWPID`, `CLONE_NEWNET`, `CLONE_NEWIPC`, `CLONE_NEWUTS`).
   - Control Groups v2 resource quotas (`memory.max`, `cpu.max`, `pids.max`).
   - Capability bounding set dropping.
   - `PR_SET_NO_NEW_PRIVS` prevents privilege escalation via setuid binaries.
   - Read-only root filesystem remounting (`--read-only`).
   - Scoped virtual network namespaces and packet masquerading.

---

## 2. Linux Capabilities Dropping

By default, Linux processes running as root (`UID 0`) inherit extensive kernel capabilities. Maclet drops dangerous system administration capabilities from the capability bounding set (`PR_CAPBND_DROP`) before executing the container process:

- `CAP_SYS_ADMIN`: Prevents unsharing namespaces, mounting filesystems, and low-level system configuration.
- `CAP_SYS_MODULE`: Prevents loading or unloading Linux kernel modules.
- `CAP_SYS_RAWIO`: Prevents raw I/O port and hardware memory manipulation.
- `CAP_SYS_BOOT`: Prevents rebooting or halting the guest kernel.
- `CAP_AUDIT_CONTROL`: Prevents disabling or tampering with kernel audit logs.

Verification can be performed from inside any running container via `/proc/self/status`, showing bits for `CapBnd` cleared of dangerous privileges.

---

## 3. Read-Only Root Filesystems

When containers are started with `--read-only`, Maclet enforces immutable root filesystems:
1. OverlayFS mounts lower, upper, and work layers normally.
2. In the container child process after `pivot_root`, the root mount (`/`) is remounted read-only:
   ```c
   mount(NULL, "/", NULL, MS_BIND | MS_REMOUNT | MS_RDONLY, NULL);
   ```
3. Any attempt to write or create files within the container fails immediately with `EROFS` (Read-only file system).

---

## 4. No New Privileges (`no_new_privs`)

Before calling `execve`, the container runner sets:
```c
prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0);
```
This guarantees that child processes or setuid/setgid binaries executed inside the container cannot acquire privileges beyond what the initial container process had.

---

## 5. Host Privileged Operations

- Invoking Apple `Virtualization.framework` requires the `com.apple.security.virtualization` entitlement signed into the macOS binary.
- All release and test binaries are signed with `codesign --sign - --entitlements entitlements.plist --force`.

