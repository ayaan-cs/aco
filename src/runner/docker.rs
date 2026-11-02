//! Docker sandbox, the preferred and portable boundary.
//!
//! Calls the `docker` binary with a fixed argument list (no shell): no network,
//! read-only root, all capabilities dropped, no-new-privileges, pid, memory and
//! CPU limits, the `nobody` user, a tmpfs `/tmp`, and the harness plus
//! candidate bind-mounted read-only at `/work`, the project snapshot at
//! `/proj` with the candidate mounted over the file it replaces. No host
//! environment is passed; only a fixed hash seed and thread-count variables.
//! One container runs all reps for a candidate, so startup is never timed.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use super::{
    parse_outcome, run_captured, sandbox_env, Backend, Limits, Outcome, RunCtx, Runner, WorkDir,
    PROJ, WORK,
};
use crate::deps::{Installed, Requirements};

const IMAGE_PIN: &str = include_str!("../../sandbox/image.txt");

/// The digest-pinned image from `sandbox/image.txt`.
pub fn pinned_image() -> String {
    IMAGE_PIN
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))
        .unwrap_or("python:3.12-slim")
        .to_string()
}

pub struct DockerRunner {
    pub bin: PathBuf,
    pub image: String,
    pub limits: Limits,
}

impl DockerRunner {
    pub fn new(limits: Limits) -> Self {
        DockerRunner {
            bin: PathBuf::from("docker"),
            image: pinned_image(),
            limits,
        }
    }

    fn quick(&self, args: &[&str], timeout: Duration) -> Result<(), String> {
        let mut cmd = Command::new(&self.bin);
        cmd.args(args);
        let c = run_captured(cmd, timeout, 64 << 10, &|| {}).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                "`docker` is not installed (not found on PATH)".to_string()
            } else {
                format!("`docker` could not be started: {e}")
            }
        })?;
        if c.timed_out {
            return Err(format!("`docker {}` timed out", args.join(" ")));
        }
        if c.status.map(|s| s.success()).unwrap_or(false) {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&c.stderr).trim().to_string())
        }
    }

    /// The daemon answers and the pinned image is present (pulled if needed).
    pub fn probe(&self) -> Result<(), String> {
        self.quick(
            &["version", "--format", "{{.Server.Version}}"],
            Duration::from_secs(10),
        )
        .map_err(|e| format!("docker daemon unavailable: {e}"))?;
        if self
            .quick(&["image", "inspect", &self.image], Duration::from_secs(20))
            .is_err()
        {
            self.quick(&["pull", &self.image], Duration::from_secs(900))
                .map_err(|e| format!("could not pull {}: {e}", self.image))?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn args(
        &self,
        name: &str,
        files: &str,
        project: &str,
        overlay: Option<&str>,
        image: &str,
        seed: u32,
    ) -> Vec<String> {
        let l = &self.limits;
        let mut a: Vec<String> = [
            "run",
            "--rm",
            "--name",
            name,
            "--network",
            "none",
            "--read-only",
            "--cap-drop",
            "ALL",
            "--security-opt",
            "no-new-privileges",
            "--user",
            "65534:65534",
            "--tmpfs",
            "/tmp:rw,nosuid,nodev,size=64m",
            "--workdir",
            "/tmp",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        a.extend([
            "--pids-limit".into(),
            l.pids.to_string(),
            "--memory".into(),
            format!("{}m", l.memory_mb),
            "--memory-swap".into(),
            format!("{}m", l.memory_mb),
            "--cpus".into(),
            format!("{}", l.cpus),
            "--ulimit".into(),
            format!("nofile={0}:{0}", l.nofile),
            "--ulimit".into(),
            format!("cpu={0}:{0}", l.cpu_secs()),
            "--mount".into(),
            format!("type=bind,source={files},target={WORK},readonly"),
            "--mount".into(),
            format!("type=bind,source={project},target={PROJ},readonly"),
        ]);
        if let Some(rel) = overlay {
            a.extend([
                "--mount".into(),
                format!("type=bind,source={files}/candidate.py,target={PROJ}/{rel},readonly"),
            ]);
        }
        // Only these variables are set; nothing from the host is passed.
        for (k, v) in sandbox_env(seed) {
            a.extend(["--env".into(), format!("{k}={v}")]);
        }
        a.extend([
            image.to_string(),
            "python3".into(),
            "-s".into(),
            "-B".into(),
            format!("{WORK}/harness.py"),
            format!("{WORK}/run.json"),
        ]);
        a
    }
}

impl Backend for DockerRunner {
    fn name(&self) -> &'static str {
        "docker"
    }

    fn exec(&self, ctx: &RunCtx, source: &str, reps: usize, seed: u32) -> Outcome {
        let nonce = super::nonce();
        let image = ctx.deps.docker_image.as_deref().unwrap_or(&self.image);
        let deps = ctx
            .deps
            .docker_image
            .as_ref()
            .map(|_| crate::deps::DOCKER_DEPS_DIR);
        let wd = match WorkDir::new(source, ctx, reps, self.limits.warmup, &nonce, deps) {
            Ok(w) => w,
            Err(e) => return Outcome::failed(format!("could not create work dir: {e}")),
        };
        let name = format!("aco-{nonce}");
        let files = wd.files().to_string_lossy().to_string();
        let project = wd.project(ctx).to_string_lossy().to_string();
        let mut cmd = Command::new(&self.bin);
        cmd.args(self.args(&name, &files, &project, wd.overlay(ctx), image, seed));
        let kill = || {
            let _ = Command::new(&self.bin)
                .args(["kill", &name])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        };
        match run_captured(cmd, self.limits.timeout, self.limits.output_cap, &kill) {
            Ok(c) => parse_outcome(&c, &nonce),
            Err(e) => Outcome::failed(format!("could not start docker: {e}")),
        }
    }

    fn prepare_deps(&self, reqs: &Requirements) -> Result<Installed, String> {
        crate::deps::install_docker(&self.bin, &self.image, reqs)
    }
}

impl Runner for DockerRunner {
    fn name(&self) -> &'static str {
        "docker"
    }
    fn run(&self, source: &str, reps: usize) -> Outcome {
        self.exec(&RunCtx::default(), source, reps, 0)
    }
    fn run_seeded(&self, source: &str, reps: usize, seed: u32) -> Outcome {
        self.exec(&RunCtx::default(), source, reps, seed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_is_pinned_by_digest() {
        let img = pinned_image();
        assert!(img.starts_with("python:3.12-slim@sha256:"), "{img}");
        assert_eq!(img.len(), "python:3.12-slim@sha256:".len() + 64);
    }

    #[test]
    fn argument_list_keeps_every_isolation_flag() {
        let d = DockerRunner::new(Limits::default());
        let a = d
            .args(
                "aco-x",
                "/tmp/aco-x/files",
                "/tmp/snap",
                Some("pkg/mod.py"),
                &d.image,
                7,
            )
            .join(" ");
        for flag in [
            "--network none",
            "--read-only",
            "--cap-drop ALL",
            "--security-opt no-new-privileges",
            "--user 65534:65534",
            "--pids-limit 64",
            "--memory 1024m",
            "--cpus 1",
            "target=/work,readonly",
            "target=/proj,readonly",
            "source=/tmp/aco-x/files/candidate.py,target=/proj/pkg/mod.py,readonly",
            "--env PYTHONHASHSEED=7",
        ] {
            assert!(a.contains(flag), "missing {flag} in {a}");
        }
        // Only the fixed sandbox variables are set, never host values.
        let envs: Vec<&str> = a
            .split(" --env ")
            .skip(1)
            .map(|s| s.split(' ').next().unwrap())
            .collect();
        assert!(
            envs.iter()
                .all(|e| e.starts_with("PYTHONHASHSEED=") || e.ends_with("_THREADS=1")),
            "{envs:?}"
        );
    }
}
