//! Emit the engine crate's resolved version as `ENGINE_VERSION` so the `describe`
//! output reports the real library version and tracks it as the lockfile moves.
//! Reading the committed Cargo.lock keeps this deterministic with the build.
//!
//! zmq-rs is unpublished and its workspace version stays at `0.1.0-dev` across
//! commits, so the version alone would merge every rev the arena ever ran into
//! one build id. The locked git commit is appended as semver build metadata
//! (`0.1.0-dev+bb3c52c`) so an archive records which commit produced a number.

use std::{env, fs, path::Path};

// The crate that IS the engine for this target.
const ENGINE_CRATE: &str = "zmq";

fn main() {
    println!("cargo:rerun-if-changed=Cargo.lock");
    let version = lock_version(ENGINE_CRATE).unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=ENGINE_VERSION={version}");
}

/// The `version` after `name = "<crate>"` in Cargo.lock, plus the short commit
/// from its `source = "git+...#<sha>"` line.
fn lock_version(crate_name: &str) -> Option<String> {
    let dir = env::var("CARGO_MANIFEST_DIR").ok()?;
    let lock = fs::read_to_string(Path::new(&dir).join("Cargo.lock")).ok()?;
    let needle = format!("name = \"{crate_name}\"");
    let mut version = None;
    let mut commit = None;
    let mut in_pkg = false;
    for line in lock.lines() {
        let line = line.trim();
        if line == needle {
            in_pkg = true;
        } else if line == "[[package]]" && in_pkg {
            break;
        } else if in_pkg && let Some(rest) = line.strip_prefix("version = \"") {
            version = Some(rest.trim_end_matches('"').to_string());
        } else if in_pkg && let Some(rest) = line.strip_prefix("source = \"git+") {
            commit = rest
                .trim_end_matches('"')
                .rsplit_once('#')
                .map(|(_, sha)| sha.chars().take(7).collect::<String>());
        }
    }
    match (version?, commit) {
        (v, Some(sha)) => Some(format!("{v}+{sha}")),
        (v, None) => Some(v),
    }
}
