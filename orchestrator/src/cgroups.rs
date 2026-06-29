//! cgroup v2 provisioning for target process isolation.
//!
//! Each target process runs inside its own leaf cgroup under the unified
//! hierarchy (`/sys/fs/cgroup`). The orchestrator writes the controller
//! interface files directly so the limits are set before the target's first
//! instruction, with no transient unconfined window.
//!
//! Hierarchy: `/sys/fs/cgroup/zmq-arena/<run>/<cell>`. Controllers are enabled
//! down the ancestor chain via `cgroup.subtree_control`; the leaf holds the
//! process and its limits. This respects the cgroup v2 "no internal processes"
//! rule: only leaves carry tasks, intermediate nodes only delegate controllers.
//!
//! Requires CAP_SYS_ADMIN (root) and a cgroup v2 host. On a systemd host the
//! root `cgroup.subtree_control` is managed by systemd; enabling controllers at
//! the root may fail, in which case run the arena inside a delegated subtree.
//! Enabling is best-effort per level and idempotent.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::config::Isolation;

const CGROUP_ROOT: &str = "/sys/fs/cgroup";
/// Controllers the arena needs delegated to leaves.
const SUBTREE_ENABLE: &str = "+cpuset +memory";

pub struct Cgroup {
    path: PathBuf,
    isolation: Isolation,
}

fn write_file(path: &Path, content: &str) -> Result<()> {
    fs::write(path, content)
        .with_context(|| format!("writing {:?} to {}", content, path.display()))
}

impl Cgroup {
    /// Build the leaf path for a named cell, e.g.
    /// `/sys/fs/cgroup/zmq-arena/<run>/<cell>`.
    pub fn new(run_id: &str, cell_id: &str, isolation: Isolation) -> Self {
        let path = Path::new(CGROUP_ROOT)
            .join("zmq-arena")
            .join(run_id)
            .join(cell_id);
        Self { path, isolation }
    }

    /// Create the leaf and enable `cpuset` + `memory` on every ancestor between
    /// the unified root and the leaf's parent, so the leaf can use them.
    pub fn create(&self) -> Result<()> {
        let root = Path::new(CGROUP_ROOT);
        let parent = self
            .path
            .parent()
            .expect("leaf cgroup always has a parent");

        // Ancestors from root down to (and including) the leaf's parent: these
        // are the nodes that must enable the controllers in subtree_control.
        let mut chain: Vec<PathBuf> = Vec::new();
        let mut cur = parent.to_path_buf();
        loop {
            chain.push(cur.clone());
            if cur == root {
                break;
            }
            cur = cur
                .parent()
                .expect("ancestor within cgroup root")
                .to_path_buf();
        }
        chain.reverse(); // root first, leaf's parent last

        for dir in &chain {
            if dir != root && !dir.exists() {
                fs::create_dir(dir).with_context(|| format!("mkdir {}", dir.display()))?;
            }
            // Idempotent and best-effort: re-enabling is harmless, and the root
            // may be systemd-managed. Errors here are not fatal; apply_limits
            // surfaces a hard failure if the controller really is unavailable.
            let _ = write_file(&dir.join("cgroup.subtree_control"), SUBTREE_ENABLE);
        }

        if !self.path.exists() {
            fs::create_dir(&self.path)
                .with_context(|| format!("mkdir {}", self.path.display()))?;
        }
        Ok(())
    }

    /// Write the resource limits into the leaf's controller interface files.
    /// `cpuset.mems` must be set for the cpuset controller to admit tasks.
    pub fn apply_limits(&self) -> Result<()> {
        write_file(&self.path.join("cpuset.cpus"), &self.isolation.cpuset_cpus)?;
        let mems = self
            .isolation
            .cpuset_mems
            .clone()
            .unwrap_or_else(|| "0".to_string());
        write_file(&self.path.join("cpuset.mems"), &mems)?;
        write_file(
            &self.path.join("memory.max"),
            &self.isolation.memory_max_bytes.to_string(),
        )?;
        Ok(())
    }

    /// Move a running process (and its future threads) into this cgroup by
    /// writing its PID to `cgroup.procs`. Do this before the target opens any
    /// socket so the pinning covers every thread it spawns.
    pub fn attach(&self, pid: u32) -> Result<()> {
        write_file(&self.path.join("cgroup.procs"), &pid.to_string())
    }

    /// High-water memory for the run record. Prefers `memory.peak` (newer
    /// kernels); falls back to `memory.current`.
    pub fn peak_memory_bytes(&self) -> Result<u64> {
        let peak = self.path.join("memory.peak");
        let src = if peak.exists() {
            peak
        } else {
            self.path.join("memory.current")
        };
        let raw = fs::read_to_string(&src)
            .with_context(|| format!("reading {}", src.display()))?;
        Ok(raw.trim().parse().unwrap_or(0))
    }
}

impl Drop for Cgroup {
    fn drop(&mut self) {
        // rmdir fails with EBUSY if the cgroup still holds tasks; the caller is
        // responsible for reaping the target first. Teardown errors are logged,
        // not propagated, because Drop cannot return Result.
        if let Err(e) = fs::remove_dir(&self.path) {
            eprintln!("cgroup teardown failed for {}: {e}", self.path.display());
        }
    }
}
