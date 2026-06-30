//! Emit the engine crate's resolved version as ENGINE_VERSION so the `describe`
//! output reports the real library version, and tracks it as the lockfile moves.
//! Reading the committed Cargo.lock keeps this deterministic with the build.

use std::{env, fs, path::Path};

// The crate that IS the engine for this target.
const ENGINE_CRATE: &str = "zeromq";

fn main() {
    println!("cargo:rerun-if-changed=Cargo.lock");
    let version = lock_version(ENGINE_CRATE).unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=ENGINE_VERSION={version}");
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
