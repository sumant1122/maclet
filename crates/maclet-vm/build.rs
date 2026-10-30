use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn find_macos_sdk() -> String {
    // Prefer known working SDK if present
    let preferred = "/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk";
    if Path::new(preferred).exists() {
        return preferred.to_string();
    }
    let output = Command::new("xcrun")
        .args(["--show-sdk-path"])
        .output()
        .expect("Failed to execute xcrun --show-sdk-path");
    String::from_utf8(output.stdout)
        .expect("Invalid UTF-8 from xcrun")
        .trim()
        .to_string()
}

fn main() {
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "macos" {
        return;
    }

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let workspace_root = manifest_dir.parent().unwrap().parent().unwrap();
    let vz_dir = workspace_root.join("virtualization").join("apple-vz");
    let swift_src = vz_dir.join("Sources").join("apple_vz.swift");
    let header_dir = vz_dir.join("include");
    let bridging_header = header_dir.join("apple_vz.h");

    println!("cargo:rerun-if-changed={}", swift_src.display());
    println!("cargo:rerun-if-changed={}", bridging_header.display());

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let lib_path = out_dir.join("libapple_vz.a");
    let sdk = find_macos_sdk();

    let status = Command::new("swiftc")
        .args([
            "-sdk",
            &sdk,
            "-import-objc-header",
            bridging_header.to_str().unwrap(),
            "-emit-library",
            "-static",
            swift_src.to_str().unwrap(),
            "-o",
            lib_path.to_str().unwrap(),
        ])
        .status()
        .expect("Failed to compile apple_vz.swift");

    if !status.success() {
        panic!("swiftc failed to compile apple_vz.swift");
    }

    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-lib=static=apple_vz");
    println!("cargo:rustc-link-lib=framework=Virtualization");
    println!("cargo:rustc-link-lib=framework=Foundation");
}
