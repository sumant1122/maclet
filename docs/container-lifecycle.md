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
         +-----> |    Running    | <------+
         |       +---------------+        |
         |               |                |
     [unpause]       [pause]              |
         |               |                |
         |               v                |
         |       +---------------+        |
         +------ |    Paused     |        |
                 +---------------+        |
                         |                |
                      [stop]              |
                         |                |
                         v                |
                 +---------------+        |
                 |    Stopped    | -------+ [restart]
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
   - The container rootfs (OverlayFS) has been constructed and mounted.
   - Namespaces (Network, UTS, IPC) are allocated.
   - cgroups v2 resource tree is initialized with memory/CPU limits.
   - The container init process is not yet launched.

2. **Running**:
   - The container process has been forked via `clone3` / `clone(CLONE_NEWPID | CLONE_NEWNS | ...)` with unshared mounts and `pivot_root` applied.
   - Process is actively executing under its designated cgroup.
   - Exit status is monitored by `maclet-guest-agent`.

3. **Paused**:
   - The cgroups v2 freezer (`cgroup.freeze = 1`) freezes all processes in the container cgroup atomically without killing them.

4. **Stopped**:
   - Container init process has terminated (either normally or via `SIGTERM` / `SIGKILL`).
   - cgroup statistics and exit code are recorded.
   - Namespaces are preserved until explicit container deletion, allowing post-mortem diagnostics.

5. **Deleted**:
   - OverlayFS unmounted.
   - cgroup v2 directory removed.
   - Virtual Ethernet pair deleted.
   - Container metadata pruned from host state store.
