# Risk Analysis & Alternative Technical Approaches

## 1. Identified Risks

### Risk 1: Hypervisor Entitlement Signing
- **Issue**: macOS restricts `Virtualization.framework` behind `com.apple.security.virtualization`. If binaries are compiled without proper codesigning, VM initialization will fail with code `VZErrorDomain Code=2`.
- **Mitigation**: Embed an automated codesigning step in the cargo build workflow (`build.rs` or Makefile) using `codesign --sign - --entitlements ... --force`.

### Risk 2: Modular vs Monolithic Guest Kernels
- **Issue**: Standard distribution kernels (like Alpine or Ubuntu) place Virtio-VSOCK and other drivers in loadable modules (`.ko`) rather than the core binary.
- **Mitigation**: Use Virtio Console (`/dev/hvc1`) for the primary host-guest control stream. Virtio console drivers are universally built into kernel images, eliminating dependency on loadable modules during early boot.

### Risk 3: Swift / Rust ABI Mismatches
- **Issue**: Swift's native ABI changes across compiler releases and lacks a stable C++ interop on all platforms.
- **Mitigation**: Keep the Swift/Rust FFI strictly C-compatible (`@_cdecl`, simple structs, opaque pointers, primitive integers, and standard C callbacks).

## 2. Alternative Technical Approaches Considered

1. **Approach: VSOCK vs Virtio Console Socketpair**
   - *Considered*: Listening for VSOCK CID 2 on macOS.
   - *Trade-off*: VSOCK requires `CONFIG_VIRTIO_VSOCK=y` compiled in or module loading before communication starts.
   - *Selection*: Virtio Console with `socketpair(AF_UNIX, ...)` is instantly available from the first microsecond of kernel execution, highly resilient, and zero-configuration.

2. **Approach: Objective-C vs Swift for Apple Virtualization Bridge**
   - *Considered*: Calling `Virtualization.framework` directly using Objective-C runtime via `objc` or `block` crates.
   - *Trade-off*: Modern Apple Virtualization APIs (such as virtiofs and socket device listeners) are ergonomic and type-safe in Swift with strong compiler checks.
   - *Selection*: A thin Swift static library with `@_cdecl` FFI exports, wrapped safely in Rust.
