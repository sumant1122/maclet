use anyhow::{anyhow, Context, Result};
use flate2::read::GzDecoder;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

const KERNEL_URL: &str =
    "https://dl-cdn.alpinelinux.org/alpine/v3.20/releases/aarch64/netboot/vmlinuz-virt";

pub fn ensure_kernel(kernel_dir: &Path) -> Result<PathBuf> {
    fs::create_dir_all(kernel_dir)?;
    let target_image = kernel_dir.join("Image");

    if target_image.exists() {
        return Ok(target_image);
    }

    // Check if test image exists locally
    let test_img = Path::new("/tmp/maclet_test/Image");
    if test_img.exists() {
        fs::copy(test_img, &target_image)?;
        return Ok(target_image);
    }

    println!("Downloading minimal Linux ARM64 kernel from Alpine mirror...");
    let temp_vmlinuz = kernel_dir.join("vmlinuz.download");

    let status = Command::new("curl")
        .args(["-sSL", KERNEL_URL, "-o", temp_vmlinuz.to_str().unwrap()])
        .status()
        .context("Failed to invoke curl to download kernel")?;

    if !status.success() {
        return Err(anyhow!("curl download failed with status {}", status));
    }

    println!("Extracting raw ARM64 Linux kernel Image...");
    extract_kernel_image(&temp_vmlinuz, &target_image)?;
    let _ = fs::remove_file(temp_vmlinuz);

    Ok(target_image)
}

pub fn extract_kernel_image(vmlinuz_path: &Path, output_path: &Path) -> Result<()> {
    let mut file = File::open(vmlinuz_path)?;
    let mut data = Vec::new();
    file.read_to_end(&mut data)?;

    // If already raw Image
    if data.len() > 64 && &data[56..60] == b"ARMd" {
        fs::write(output_path, &data)?;
        return Ok(());
    }

    // Search for gzip header 0x1f, 0x8b, 0x08
    let gzip_magic = [0x1fu8, 0x8bu8, 0x08u8];
    let offset = data
        .windows(3)
        .position(|w| w == gzip_magic)
        .ok_or_else(|| anyhow!("Could not find gzip header in kernel binary"))?;

    let mut decoder = GzDecoder::new(&data[offset..]);
    let mut decompressed = Vec::new();
    decoder
        .read_to_end(&mut decompressed)
        .context("Failed to decompress gzip kernel stream")?;

    if decompressed.len() > 64 && &decompressed[56..60] == b"ARMd" {
        let mut out = File::create(output_path)?;
        out.write_all(&decompressed)?;
        Ok(())
    } else {
        Err(anyhow!(
            "Decompressed kernel did not contain ARM64 kernel magic (ARMd)"
        ))
    }
}
