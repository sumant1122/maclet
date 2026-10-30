use crate::cpio::CpioWriter;
use anyhow::{anyhow, Context, Result};
use flate2::write::GzEncoder;
use flate2::Compression;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn build_guest_agent(workspace_root: &Path) -> Result<PathBuf> {
    println!("Building static Linux ARM64 guest agent (aarch64-unknown-linux-musl)...");

    // Find rust-lld or gcc-ld in toolchain
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

pub fn create_initramfs(agent_binary: &Path, output_path: &Path) -> Result<()> {
    println!("Packaging initramfs with /init guest agent...");
    let mut file = File::open(agent_binary)
        .context("Failed to open guest agent binary")?;
    let mut agent_data = Vec::new();
    file.read_to_end(&mut agent_data)?;

    let out_file = File::create(output_path)
        .context("Failed to create initrd.img output file")?;
    let gz = GzEncoder::new(out_file, Compression::default());
    let mut cpio = CpioWriter::new(gz);

    // Standard directories (mode 0o040755)
    for dir in &["proc", "sys", "dev", "tmp", "bin", "etc"] {
        cpio.write_entry(dir, 0o040755, 0, 0, &[])?;
    }

    // Write /init
    cpio.write_entry("init", 0o100755, 0, 0, &agent_data)?;

    let mut gz_done = cpio.finish()?;
    use std::io::Write;
    gz_done.flush()?;

    println!("Created initramfs at {}", output_path.display());
    Ok(())
}
