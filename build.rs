//! Build script: snapshot the noVNC web client into `$OUT_DIR/web`.
//!
//! The client is resolved from, in order:
//!
//! 1. `$NOVNC_CLIENT_DIR`
//! 2. `./novnc-client` (a checkout managed by `build.bat --update-client`)
//! 3. `./original-novnc` (the snapshot bundled with this repository)
//!
//! Only the runtime entry points are copied (not `docs/`, `tests/`, `.git/`),
//! which keeps the embedded payload around 2.5 MB.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

/// Files and directories that make up the served client.
const CLIENT_ENTRIES: &[&str] = &[
    "app",
    "core",
    "vendor",
    "vnc.html",
    "vnc_lite.html",
    "defaults.json",
    "mandatory.json",
    "package.json",
];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=NOVNC_CLIENT_DIR");
    // Watch the candidate checkouts that exist right now, so editing the
    // client rebuilds the binary. A newly added checkout is picked up through
    // NOVNC_CLIENT_DIR, which build.bat always sets.
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    for candidate in ["novnc-client", "original-novnc"] {
        let dir = manifest.join(candidate);
        if dir.exists() {
            println!("cargo:rerun-if-changed={}", dir.display());
        }
    }

    let root = resolve_root();
    println!("cargo:rerun-if-changed={}", root.display());
    eprintln!("embedding noVNC client from {}", root.display());

    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is set by Cargo")).join("web");
    if out.exists() {
        fs::remove_dir_all(&out).unwrap_or_else(|e| panic!("cannot clean {}: {e}", out.display()));
    }

    for entry in CLIENT_ENTRIES {
        let src = root.join(entry);
        if !src.exists() {
            panic!(
                "noVNC client file `{}` is missing; set NOVNC_CLIENT_DIR or clone noVNC into ./novnc-client",
                src.display()
            );
        }
        copy(&src, &out.join(entry));
    }

    let package = fs::read_to_string(root.join("package.json")).unwrap_or_default();
    let version = parse_version(&package).unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=NOVNC_CLIENT_VERSION={version}");
}

fn resolve_root() -> PathBuf {
    if let Some(dir) = env::var_os("NOVNC_CLIENT_DIR") {
        let dir = PathBuf::from(dir);
        if !dir.join("vnc.html").is_file() {
            panic!(
                "NOVNC_CLIENT_DIR={} does not look like a noVNC checkout (vnc.html missing)",
                dir.display()
            );
        }
        return dir;
    }

    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    for candidate in ["novnc-client", "original-novnc"] {
        let dir = manifest.join(candidate);
        if dir.join("vnc.html").is_file() {
            return dir;
        }
    }

    panic!(
        "no noVNC client found; clone it with `git clone --depth 1 https://github.com/novnc/noVNC.git novnc-client` \
         or set NOVNC_CLIENT_DIR"
    );
}

fn copy(src: &Path, dst: &Path) {
    let meta =
        fs::symlink_metadata(src).unwrap_or_else(|e| panic!("cannot read {}: {e}", src.display()));
    if meta.is_dir() {
        fs::create_dir_all(dst).unwrap_or_else(|e| panic!("cannot create {}: {e}", dst.display()));
        let mut entries: Vec<_> = fs::read_dir(src)
            .unwrap_or_else(|e| panic!("cannot list {}: {e}", src.display()))
            .map(|entry| entry.unwrap_or_else(|e| panic!("cannot read {}: {e}", src.display())))
            .map(|entry| entry.path())
            .collect();
        entries.sort();
        for entry in entries {
            let name = entry
                .file_name()
                .expect("client entries always have a file name");
            copy(&entry, &dst.join(name));
        }
    } else {
        let data = fs::read(src).unwrap_or_else(|e| panic!("cannot read {}: {e}", src.display()));
        fs::write(dst, data).unwrap_or_else(|e| panic!("cannot write {}: {e}", dst.display()));
    }
}

/// Extract `"version": "x.y.z"` from a `package.json` without a JSON dependency.
fn parse_version(package: &str) -> Option<String> {
    let key = "\"version\"";
    let start = package.find(key)? + key.len();
    let rest = &package[start..];
    let colon = rest.find(':')? + 1;
    let rest = rest[colon..].trim_start();
    let rest = rest.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}
