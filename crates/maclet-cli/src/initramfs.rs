use crate::cpio::CpioWriter;
use anyhow::{anyhow, Context, Result};
use flate2::write::GzEncoder;
use flate2::Compression;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

const ROOTFS_URL: &str =
    "https://dl-cdn.alpinelinux.org/alpine/v3.20/releases/aarch64/alpine-minirootfs-3.20.0-aarch64.tar.gz";

pub fn ensure_rootfs_tar(kernel_dir: &Path) -> Result<PathBuf> {
    let target_tar = kernel_dir.join("rootfs.tar.gz");
    if target_tar.exists() {
        return Ok(target_tar);
    }

    println!("Downloading minimal Linux root filesystem (Alpine ARM64 minirootfs)...");
    let status = Command::new("curl")
        .args(["-sSL", ROOTFS_URL, "-o", target_tar.to_str().unwrap()])
        .status()
        .context("Failed to download minirootfs")?;

    if !status.success() {
        return Err(anyhow!("Failed to download rootfs with status {}", status));
    }

    Ok(target_tar)
}

pub fn build_guest_agent(workspace_root: &Path) -> Result<PathBuf> {
    println!("Building static Linux ARM64 guest agent (aarch64-unknown-linux-musl)...");

    let sysroot_out = Command::new("rustc")
        .args(["--print", "sysroot"])
        .output()
        .context("Failed to get rustc sysroot")?;
    let sysroot = String::from_utf8(sysroot_out.stdout)?.trim().to_string();

    let lld_path = PathBuf::from(&sysroot)
        .join("lib/rustlib/aarch64-apple-darwin/bin/gcc-ld/ld.lld");

    let mut cmd = Command::new("cargo");
    cmd.current_dir(workspace_root);
    cmd.args([
        "build",
        "--package",
        "maclet-guest-agent",
        "--target",
        "aarch64-unknown-linux-musl",
        "--release",
    ]);

    if lld_path.exists() {
        cmd.env(
            "RUSTFLAGS",
            format!("-C linker={}", lld_path.display()),
        );
    }

    let status = cmd.status().context("Failed to execute cargo build for guest agent")?;
    if !status.success() {
        return Err(anyhow!("Guest agent compilation failed with exit status {}", status));
    }

    let binary = workspace_root
        .join("target/aarch64-unknown-linux-musl/release/maclet-guest-agent");
    if !binary.exists() {
        return Err(anyhow!("Built guest agent binary not found at {}", binary.display()));
    }

    Ok(binary)
}

pub fn create_initramfs(
    agent_binary: &Path,
    rootfs_tar: &Path,
    modules_dir: Option<&Path>,
    output_path: &Path,
) -> Result<()> {
    println!("Packaging initramfs with /init and rootfs.tar.gz...");
    let mut file = File::open(agent_binary)
        .context("Failed to open guest agent binary")?;
    let mut agent_data = Vec::new();
    file.read_to_end(&mut agent_data)?;

    let mut rootfs_file = File::open(rootfs_tar)
        .context("Failed to open rootfs.tar.gz")?;
    let mut rootfs_data = Vec::new();
    rootfs_file.read_to_end(&mut rootfs_data)?;

    let out_file = File::create(output_path)
        .context("Failed to create initrd.img output file")?;
    let gz = GzEncoder::new(out_file, Compression::default());
    let mut cpio = CpioWriter::new(gz);

    // Standard directories (mode 0o040755)
    for dir in &[
        "proc", "sys", "dev", "tmp", "bin", "etc", "var", "var/lib", "var/lib/maclet",
        "lib", "lib/modules",
    ] {
        cpio.write_entry(dir, 0o040755, 0, 0, &[])?;
    }

    // Write /init
    cpio.write_entry("init", 0o100755, 0, 0, &agent_data)?;

    // Write /rootfs.tar.gz
    cpio.write_entry("rootfs.tar.gz", 0o100644, 0, 0, &rootfs_data)?;

    // Write essential modules if present
    if let Some(mod_dir) = modules_dir {
        for mod_name in &[
            "overlay.ko",
            "squashfs.ko",
            "loop.ko",
            "virtio_blk.ko",
            "af_packet.ko",
            "failover.ko",
            "net_failover.ko",
            "virtio_net.ko",
        ] {
            let p = mod_dir.join(mod_name);
            if p.exists() {
                if let Ok(mut mf) = File::open(&p) {
                    let mut mod_data = Vec::new();
                    if mf.read_to_end(&mut mod_data).is_ok() {
                        cpio.write_entry(&format!("lib/modules/{}", mod_name), 0o100644, 0, 0, &mod_data)?;
                    }
                }
            }
        }
    }

    let mut gz_done = cpio.finish()?;
    use std::io::Write;
    gz_done.flush()?;

    println!("Created initramfs at {}", output_path.display());
    Ok(())
}
