//! Docker sandbox, the preferred and portable boundary.
//!
//! Calls the `docker` binary with a fixed argument list (no shell): no network,
//! read-only root, all capabilities dropped, no-new-privileges, pid, memory and
//! CPU limits, the `nobody` user, a tmpfs `/tmp`, and the harness plus
//! candidate bind-mounted read-only at `/work`. No host environment is passed.
//! One container runs all reps for a candidate, so startup is never timed.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use super::{parse_outcome, run_captured, Limits, Outcome, Runner, WorkDir};

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
        let c = run_captured(cmd, timeout, 64 << 10, &|| {})
            .map_err(|e| format!("`docker` could not be started: {e}"))?;
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

    fn args(&self, name: &str, files: &str, reps: usize, nonce: &str) -> Vec<String> {
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
            format!("type=bind,source={files},target=/work,readonly"),
            self.image.clone(),
            "python3".into(),
            "-I".into(),
            "-B".into(),
            "/work/harness.py".into(),
            "/work/candidate.py".into(),
            reps.to_string(),
            l.warmup.to_string(),
            nonce.into(),
        ]);
        a
    }
}

impl Runner for DockerRunner {
    fn name(&self) -> &'static str {
        "docker"
    }

    fn run(&self, source: &str, reps: usize) -> Outcome {
        let wd = match WorkDir::new(source) {
            Ok(w) => w,
            Err(e) => return Outcome::failed(format!("could not create work dir: {e}")),
        };
        let nonce = super::nonce();
        let name = format!("aco-{nonce}");
        let files = wd.files().to_string_lossy().to_string();
        let mut cmd = Command::new(&self.bin);
        cmd.args(self.args(&name, &files, reps, &nonce));
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
        let a = d.args("aco-x", "/tmp/aco-x/files", 5, "n").join(" ");
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
        ] {
            assert!(a.contains(flag), "missing {flag} in {a}");
        }
        assert!(!a.contains("--env"), "no host env may be passed");
    }
}
