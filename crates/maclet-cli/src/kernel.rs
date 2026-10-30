use anyhow::{anyhow, Context, Result};
use flate2::read::GzDecoder;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

const KERNEL_URL: &str =
    "https://dl-cdn.alpinelinux.org/alpine/v3.20/releases/aarch64/netboot/vmlinuz-virt";

const MODLOOP_URL: &str =
    "https://dl-cdn.alpinelinux.org/alpine/v3.20/releases/aarch64/netboot/modloop-virt";

const INITRAMFS_VIRT_URL: &str =
    "https://dl-cdn.alpinelinux.org/alpine/v3.20/releases/aarch64/netboot/initramfs-virt";

pub fn ensure_modloop(kernel_dir: &Path) -> Result<PathBuf> {
    fs::create_dir_all(kernel_dir)?;
    let target_modloop = kernel_dir.join("modloop.squashfs");

    if target_modloop.exists() {
        return Ok(target_modloop);
    }

    println!("Downloading Linux kernel modules (modloop) from Alpine mirror...");
    let temp_modloop = kernel_dir.join("modloop.download");
    let status = Command::new("curl")
        .args(["-sSL", MODLOOP_URL, "-o", temp_modloop.to_str().unwrap()])
        .status()
        .context("Failed to download modloop")?;

    if !status.success() {
        return Err(anyhow!("curl download failed with status {}", status));
    }

    fs::rename(temp_modloop, &target_modloop)?;
    Ok(target_modloop)
}

pub fn ensure_essential_modules(kernel_dir: &Path) -> Result<PathBuf> {
    let modules_dir = kernel_dir.join("modules");
    fs::create_dir_all(&modules_dir)?;

    let overlay_ko = modules_dir.join("overlay.ko");
    let squashfs_ko = modules_dir.join("squashfs.ko");
    let loop_ko = modules_dir.join("loop.ko");
    let virtio_blk_ko = modules_dir.join("virtio_blk.ko");
    let af_packet_ko = modules_dir.join("af_packet.ko");
    let virtio_net_ko = modules_dir.join("virtio_net.ko");
    let net_failover_ko = modules_dir.join("net_failover.ko");
    let failover_ko = modules_dir.join("failover.ko");

    if overlay_ko.exists()
        && squashfs_ko.exists()
        && loop_ko.exists()
        && virtio_blk_ko.exists()
        && af_packet_ko.exists()
        && virtio_net_ko.exists()
        && net_failover_ko.exists()
        && failover_ko.exists()
    {
        return Ok(modules_dir);
    }

    println!("Downloading base kernel modules (OverlayFS, SquashFS, loop, virtio_net, af_packet)...");
    let temp_initramfs = kernel_dir.join("initramfs-virt.download");
    let status = Command::new("curl")
        .args([
            "-sSL",
            INITRAMFS_VIRT_URL,
            "-o",
            temp_initramfs.to_str().unwrap(),
        ])
        .status()
        .context("Failed to download initramfs-virt")?;

    if !status.success() {
        return Err(anyhow!("Failed to download initramfs-virt with status {}", status));
    }

    let file = File::open(&temp_initramfs)?;
    let mut decoder = GzDecoder::new(file);
    let mut cpio_data = Vec::new();
    decoder
        .read_to_end(&mut cpio_data)
        .context("Failed to decompress initramfs-virt cpio")?;

    extract_file_from_cpio(&cpio_data, "overlay.ko", &overlay_ko)?;
    extract_file_from_cpio(&cpio_data, "squashfs.ko", &squashfs_ko)?;
    extract_file_from_cpio(&cpio_data, "loop.ko", &loop_ko)?;
    extract_file_from_cpio(&cpio_data, "virtio_blk.ko", &virtio_blk_ko)?;
    extract_file_from_cpio(&cpio_data, "af_packet.ko", &af_packet_ko)?;
    extract_file_from_cpio(&cpio_data, "failover.ko", &failover_ko)?;
    extract_file_from_cpio(&cpio_data, "net_failover.ko", &net_failover_ko)?;
    extract_file_from_cpio(&cpio_data, "virtio_net.ko", &virtio_net_ko)?;

    let _ = fs::remove_file(temp_initramfs);
    Ok(modules_dir)
}

fn extract_file_from_cpio(cpio_data: &[u8], filename_pattern: &str, output_path: &Path) -> Result<()> {
    let mut offset = 0;
    while offset + 110 <= cpio_data.len() {
        if &cpio_data[offset..offset + 6] != b"070701" {
            break;
        }
        let filesize = usize::from_str_radix(
            std::str::from_utf8(&cpio_data[offset + 54..offset + 62])?,
            16,
        )?;
        let namesize = usize::from_str_radix(
            std::str::from_utf8(&cpio_data[offset + 94..offset + 102])?,
            16,
        )?;

        offset += 110;
        if offset + namesize > cpio_data.len() {
            break;
        }

        let name = std::str::from_utf8(&cpio_data[offset..offset + namesize - 1])?;
        offset += namesize;
        if offset % 4 != 0 {
            offset += 4 - (offset % 4);
        }

        if name == "TRAILER!!!" {
            break;
        }

        if offset + filesize > cpio_data.len() {
            break;
        }

        let suffix = format!("/{}", filename_pattern);
        if name.ends_with(&suffix) || name == filename_pattern {
            fs::write(output_path, &cpio_data[offset..offset + filesize])?;
            return Ok(());
        }

        offset += filesize;
        if offset % 4 != 0 {
            offset += 4 - (offset % 4);
        }
    }
    Err(anyhow!("Could not find {} in cpio archive", filename_pattern))
}

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
