# Maclet Security Boundaries and Privilege Model

## 1. Two-Tier Privilege Architecture

Maclet enforces isolation at two distinct boundaries:
1. **Hypervisor Isolation (Hardware Level)**:
   - The Linux VM executes in ARM Exception Level 1 (EL1), while macOS executes the hypervisor in EL2 (hardware virtualization).
   - Even if a container escapes root inside Linux, it is confined to the guest VM kernel.
   - Host filesystem access is mediated strictly via VirtioFS with designated read-only or scoped mount flags.

2. **Container Isolation (Linux Kernel Level)**:
   - Namespaces (`CLONE_NEWNS`, `CLONE_NEWPID`, `CLONE_NEWNET`, `CLONE_NEWIPC`, `CLONE_NEWUTS`, `CLONE_NEWUSER`).
   - Control Groups v2 resource quotas.
   - Capability dropping (`CAP_SYS_ADMIN`, `CAP_NET_ADMIN`, `CAP_SYS_RAWIO` revoked by default).
   - Seccomp system call filtering.
   - Read-only `/proc/sys`, `/sys`, and `/dev` masked paths.

## 2. Privileged Operations

### Host Privileged Operations:
- Invoking Apple `Virtualization.framework` requires the `com.apple.security.virtualization` entitlement.
- Accessing host ports below 1024 requires root or user-space port redirection on macOS.

### Guest Privileged Operations:
- `maclet-guest-agent` runs as PID 1 / root inside the guest VM.
- Containers drop capabilities before `execve`:
  - `no_new_privs` bit set via `prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0)`.
  - Default seccomp filter denying dangerous syscalls (`kexec_load`, `reboot`, `sysfs`, `iopl`).
