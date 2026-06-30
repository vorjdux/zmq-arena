//! Emit the binding crate's resolved version as BINDING_VERSION so `describe`
//! reports which rust-zmq release produced the numbers. The underlying libzmq
//! version is read at runtime from zmq::version(), since that is the engine.

use std::{env, fs, path::Path};

// The FFI binding crate (not the engine; libzmq is the engine, queried at runtime).
const BINDING_CRATE: &str = "zmq";

fn main() {
    println!("cargo:rerun-if-changed=Cargo.lock");
    let version = lock_version(BINDING_CRATE).unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=BINDING_VERSION={version}");
}

/// Find the `version` that immediately follows `name = "<crate>"` in Cargo.lock.
fn lock_version(crate_name: &str) -> Option<String> {
    let dir = env::var("CARGO_MANIFEST_DIR").ok()?;
    let lock = fs::read_to_string(Path::new(&dir).join("Cargo.lock")).ok()?;
    let needle = format!("name = \"{crate_name}\"");
    let mut in_pkg = false;
    for line in lock.lines() {
        let line = line.trim();
        if line == needle {
            in_pkg = true;
        } else if in_pkg {
            if let Some(rest) = line.strip_prefix("version = \"") {
                return Some(rest.trim_end_matches('"').to_string());
            }
        }
    }
    None
}
