use std::path::{Path, PathBuf};

fn collect(path: &Path, paths: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(path).expect("read source directory") {
        let entry = entry.expect("source entry");
        let path = entry.path();
        if entry.file_type().unwrap().is_dir() {
            collect(&path, paths);
        } else {
            paths.push(path);
        }
    }
}

fn main() {
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=scripts/semantic_sidecar.py");
    let mut paths = vec![
        PathBuf::from("Cargo.toml"),
        PathBuf::from("scripts/semantic_sidecar.py"),
        PathBuf::from("build.rs"),
    ];
    collect(Path::new("src"), &mut paths);
    paths.sort();
    // Deterministic identity for diagnosing mixed adapters/readers, not a security digest.
    let mut fingerprint = 0xcbf29ce484222325_u64;
    for path in paths {
        for byte in path
            .to_string_lossy()
            .bytes()
            .chain(std::fs::read(&path).unwrap())
        {
            fingerprint = (fingerprint ^ u64::from(byte)).wrapping_mul(0x100000001b3);
        }
    }
    println!("cargo:rustc-env=AWI_BUILD_ID={fingerprint:016x}");
}
