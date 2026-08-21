//! Per-run network namespace for the `tcp_netns` transport.
//!
//! Loopback in the host namespace is shared with everything else on the
//! machine: another process's traffic, the machine's own background chatter,
//! and any conntrack or qdisc state the host happens to carry. A benchmark that
//! calls itself isolated should not be measuring on it.
//!
//! A namespace of its own gives each run a private loopback with nothing else on
//! it. Both peers of a cell must land in the SAME namespace or they cannot reach
//! each other, so this creates one named namespace per run and spawns every
//! `tcp_netns` target inside it, rather than unsharing per process.
//!
//! It needs root, like cgroups and perf, and degrades the same way: without it
//! the cells run on host loopback and the run records that they did, so a reader
//! can tell an isolated run from one that only asked to be.

use std::process::{Command, Stdio};

/// A named network namespace that lives as long as this value.
#[derive(Debug)]
pub struct NetNs {
    name: String,
}

/// Run an `ip` subcommand, returning its stderr on failure so the caller can
/// explain why the namespace is unavailable rather than just that it is.
fn ip(args: &[&str]) -> Result<(), String> {
    let out = Command::new("ip")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("running ip: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
}

impl NetNs {
    /// Create the namespace and bring its loopback up.
    ///
    /// A fresh namespace has `lo` present but DOWN, so a target binding
    /// 127.0.0.1 inside it would fail with EADDRNOTAVAIL. Bringing it up is not
    /// optional setup, it is what makes the namespace usable at all.
    pub fn create(run_id: &str) -> Result<Self, String> {
        // Namespace names are filenames under /var/run/netns, so keep them to
        // characters that cannot surprise the filesystem.
        let safe: String = run_id
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        let name = format!("zmqarena-{safe}");

        // A leftover namespace from a killed run would otherwise make this fail.
        let _ = ip(&["netns", "del", &name]);
        ip(&["netns", "add", &name])?;
        if let Err(e) = ip(&["netns", "exec", &name, "ip", "link", "set", "lo", "up"]) {
            let _ = ip(&["netns", "del", &name]);
            return Err(format!("bringing lo up: {e}"));
        }
        Ok(Self { name })
    }

    /// Build a command that runs `binary` inside the namespace.
    pub fn command(&self, binary: &std::path::Path) -> Command {
        let mut cmd = Command::new("ip");
        cmd.arg("netns").arg("exec").arg(&self.name).arg(binary);
        cmd
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

impl Drop for NetNs {
    fn drop(&mut self) {
        // Namespaces are persistent kernel objects: one left behind outlives the
        // process that made it and the next run would collide with it.
        let _ = ip(&["netns", "del", &self.name]);
    }
}
