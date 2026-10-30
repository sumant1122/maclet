# Container Lifecycle State Machine

## 1. Lifecycle State Diagram

```
                 +---------------+
                 |    Created    |
                 +---------------+
                         |
                   [start_container]
                         |
                         v
                 +---------------+
                 |    Running    |
                 +---------------+
                         |
                      [stop]
                         |
                         v
                 +---------------+
                 |    Stopped    |
                 +---------------+
                         |
                     [delete]
                         |
                         v
                 +---------------+
                 |    Deleted    |
                 +---------------+
```

## 2. State Definitions and Transitions

1. **Created**:
   - The container rootfs (OverlayFS) has been constructed and mounted under `/var/lib/maclet/overlay/<id>/merged`.
   - Dedicated upper and work directories are isolated per container.
   - Namespaces (Network, UTS, IPC, Mount, PID) are prepared.
   - cgroups v2 resource tree is initialized with memory/CPU/PIDs limits under `/sys/fs/cgroup/maclet/<id>`.
   - The container init process is not yet launched.

2. **Running**:
   - The container process has been forked via `clone(CLONE_NEWPID | CLONE_NEWNS | CLONE_NEWUTS | CLONE_NEWIPC)`.
   - Child process executes `pivot_root` into `/var/lib/maclet/overlay/<id>/merged`, unmounts the old root, and mounts fresh instances of `/proc`, `/sys`, and `/dev`.
   - `cgroup.procs` contains the container PID.
   - Process is actively executing under its designated cgroup.
   - Exit status is monitored by `maclet-guest-agent` via asynchronous supervisor threads and `waitpid`.

3. **Stopped**:
   - Container init process has terminated (either normally with exit code or via `SIGTERM` / `SIGKILL`).
   - cgroup statistics and exit code are persisted to `/var/lib/maclet/containers/<id>/meta.json`.
   - OverlayFS and container logs remain intact for inspection (`maclet logs`, `maclet inspect`).

4. **Deleted**:
   - OverlayFS unmounted from `/var/lib/maclet/overlay/<id>/merged`.
   - Upper and work directories cleaned up from disk.
   - cgroup v2 directory `/sys/fs/cgroup/maclet/<id>` removed via `rmdir`.
   - Virtual Ethernet pair (`veth_<id>`) and netns iptables rules deleted.
   - Container metadata pruned from host state store.

## 3. Storage & OverlayFS Lifecycle

Maclet manages container filesystems via Linux OverlayFS:
- **Base / Image Layer (`lowerdir`)**: Read-only image rootfs extracted at `/var/lib/maclet/rootfs`.
- **Upper Layer (`upperdir`)**: Writable layer per container at `/var/lib/maclet/overlay/<id>/upper`.
- **Work Layer (`workdir`)**: Internal scratch space required by OverlayFS at `/var/lib/maclet/overlay/<id>/work`.
- **Merged View (`merged`)**: Target mountpoint at `/var/lib/maclet/overlay/<id>/merged` presented as the container rootfs.

When the container writes or modifies files, the copy-on-write (COW) mechanism writes exclusively to the container's private `upper` directory, leaving the base rootfs and other containers untouched.
On `maclet rm -f <id>`, the merged mount is lazily unmounted (`umount2(MNT_DETACH)`) and the upper and work directories are deleted.

