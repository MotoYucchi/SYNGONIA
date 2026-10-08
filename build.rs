use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=native/spout_wrapper.cpp");
    println!("cargo:rerun-if-changed=syngonia_spout.dll");
    println!("cargo:rerun-if-changed=SpoutLibrary.dll");
    println!("cargo:rerun-if-changed=SpoutDX.dll");

    // Copy DLLs to target folder if available
    let out_dir = env::var("OUT_DIR").unwrap_or_default();
    if !out_dir.is_empty() {
        let out_path = PathBuf::from(out_dir);
        // Navigate up to target/debug or target/release
        if let Some(target_dir) = out_path.ancestors().nth(3) {
            copy_if_exists("syngonia_spout.dll", target_dir);
            copy_if_exists("SpoutLibrary.dll", target_dir);
            copy_if_exists("SpoutDX.dll", target_dir);
        }
    }
}

fn copy_if_exists(src_name: &str, target_dir: &Path) {
    let src = Path::new(src_name);
    if src.exists() {
        let dest = target_dir.join(src_name);
        let _ = fs::copy(src, dest);
    }
}
