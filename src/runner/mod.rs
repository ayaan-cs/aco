//! Runners execute a candidate inside a sandbox under the timing harness.

use serde::Serialize;

/// What one sandboxed execution produced.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Outcome {
    /// Captured stdout of the first timed rep.
    pub stdout: String,
    /// True when `stdout` was cut at the harness cap.
    pub stdout_truncated: bool,
    /// SHA-256 of the full stdout, computed inside the sandbox.
    pub stdout_sha256: String,
    /// Per-rep wall times in nanoseconds, measured inside the sandbox.
    pub times_ns: Vec<u64>,
    /// Median of `times_ns`.
    pub median_ns: u64,
    /// Peak resident set size in KiB, as reported by the sandbox.
    pub peak_rss_kb: u64,
    /// The wall-clock limit killed the run.
    pub timed_out: bool,
    /// Exception, sandbox failure, or harness error.
    pub error: Option<String>,
}

impl Outcome {
    pub fn ok(&self) -> bool {
        !self.timed_out && self.error.is_none() && !self.times_ns.is_empty()
    }

    pub fn failed(msg: impl Into<String>) -> Self {
        Outcome {
            error: Some(msg.into()),
            ..Default::default()
        }
    }
}

/// Executes `source` with `reps` timed repetitions after the configured warmup.
pub trait Runner: Send + Sync {
    fn name(&self) -> &'static str;
    fn run(&self, source: &str, reps: usize) -> Outcome;
}

pub mod docker;
pub mod unshare;

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Deserialize;

pub use docker::DockerRunner;
pub use unshare::UnshareRunner;

/// The timing harness, mounted read-only next to the candidate.
pub const HARNESS: &str = include_str!("../harness.py");

/// Resource limits applied to every sandboxed run.
#[derive(Clone, Debug)]
pub struct Limits {
    /// Wall-clock limit for one sandbox run (warmup plus all reps).
    pub timeout: Duration,
    pub memory_mb: u64,
    pub cpus: f64,
    pub pids: u64,
    pub nofile: u64,
    pub fsize_mb: u64,
    /// Bytes of stdout/stderr kept from the sandbox.
    pub output_cap: usize,
    /// Untimed runs before the timed reps.
    pub warmup: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            timeout: Duration::from_secs(60),
            memory_mb: 1024,
            cpus: 1.0,
            pids: 64,
            nofile: 64,
            fsize_mb: 16,
            output_cap: 4 << 20,
            warmup: 1,
        }
    }
}

impl Limits {
    pub(crate) fn cpu_secs(&self) -> u64 {
        self.timeout.as_secs() + 1
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum SandboxKind {
    /// Docker if it works, else unshare on Linux.
    Auto,
    Docker,
    Unshare,
}

/// Pick a sandbox. Fails closed: never falls back to running unsandboxed.
pub fn select(kind: SandboxKind, limits: Limits) -> Result<Box<dyn Runner>, String> {
    match kind {
        SandboxKind::Docker => {
            let d = DockerRunner::new(limits);
            d.probe()?;
            Ok(Box::new(d))
        }
        SandboxKind::Unshare => {
            let u = UnshareRunner::new(limits)?;
            u.probe()?;
            Ok(Box::new(u))
        }
        SandboxKind::Auto => {
            let d = DockerRunner::new(limits.clone());
            let docker_err = match d.probe() {
                Ok(()) => return Ok(Box::new(d)),
                Err(e) => e,
            };
            if !cfg!(target_os = "linux") {
                return Err(format!(
                    "no sandbox available. docker: {docker_err}. unshare: the namespace fallback is Linux-only. Install Docker."
                ));
            }
            match UnshareRunner::new(limits).and_then(|u| u.probe().map(|_| u)) {
                Ok(u) => Ok(Box::new(u)),
                Err(unshare_err) => Err(format!(
                    "no sandbox available, refusing to run candidates.\n  docker: {docker_err}\n  unshare: {unshare_err}"
                )),
            }
        }
    }
}

static ABORT: AtomicBool = AtomicBool::new(false);

/// Kill every in-flight sandbox run and fail new ones. Used on Ctrl-C so no
/// sandbox outlives `aco`. Process-wide and one-way.
pub fn abort_all() {
    ABORT.store(true, Ordering::SeqCst);
}

/// True once [`abort_all`] was called.
pub fn aborted() -> bool {
    ABORT.load(Ordering::SeqCst)
}

// ------------------------------------------------------------ internals

pub(crate) fn nonce() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut h = RandomState::new().build_hasher();
    h.write_u64(COUNTER.fetch_add(1, Ordering::SeqCst));
    h.write_u32(std::process::id());
    h.write_u128(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    );
    format!("{:016x}", h.finish())
}

/// A private scratch directory holding `files/harness.py`, `files/candidate.py`
/// and an empty `root/` mount point. Removed on drop.
pub(crate) struct WorkDir {
    pub dir: PathBuf,
}

impl WorkDir {
    pub fn new(source: &str) -> std::io::Result<Self> {
        let dir = std::env::temp_dir().join(format!("aco-{}-{}", std::process::id(), nonce()));
        std::fs::create_dir_all(dir.join("files"))?;
        std::fs::create_dir_all(dir.join("root"))?;
        std::fs::write(dir.join("files/harness.py"), HARNESS)?;
        std::fs::write(dir.join("files/candidate.py"), source)?;
        Ok(WorkDir { dir })
    }
    pub fn files(&self) -> PathBuf {
        self.dir.join("files")
    }
    pub fn root(&self) -> PathBuf {
        self.dir.join("root")
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

pub(crate) struct Captured {
    pub status: Option<ExitStatus>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub timed_out: bool,
    pub aborted: bool,
}

fn read_capped(mut r: impl Read, cap: usize) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match r.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if buf.len() < cap {
                    let take = n.min(cap - buf.len());
                    buf.extend_from_slice(&chunk[..take]);
                }
            }
        }
    }
    buf
}

/// Spawn `cmd` in its own process group with piped, capped output and a
/// wall-clock limit. On timeout `on_timeout` runs, then the group is killed.
pub(crate) fn run_captured(
    mut cmd: Command,
    timeout: Duration,
    cap: usize,
    on_timeout: &dyn Fn(),
) -> std::io::Result<Captured> {
    use std::os::unix::process::CommandExt;
    if aborted() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Interrupted,
            "cancelled",
        ));
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = cmd.spawn()?;
    let out = child.stdout.take().expect("piped stdout");
    let err = child.stderr.take().expect("piped stderr");
    let t_out = thread::spawn(move || read_capped(out, cap));
    let t_err = thread::spawn(move || read_capped(err, cap));
    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let mut was_aborted = false;
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break Some(s);
        }
        let abort = aborted();
        if abort || Instant::now() >= deadline {
            timed_out = !abort;
            was_aborted = abort;
            on_timeout();
            let _ = Command::new("kill")
                .args(["-KILL", "--", &format!("-{}", child.id())])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            let _ = child.kill();
            break child.wait().ok();
        }
        thread::sleep(Duration::from_millis(5));
    };
    Ok(Captured {
        status,
        stdout: t_out.join().unwrap_or_default(),
        stderr: t_err.join().unwrap_or_default(),
        timed_out,
        aborted: was_aborted,
    })
}

#[derive(Deserialize)]
struct HarnessResult {
    times_ns: Vec<u64>,
    error: Option<String>,
    stdout: String,
    stdout_truncated: bool,
    stdout_sha256: String,
    maxrss_kb: u64,
    cgroup_peak_bytes: Option<u64>,
}

fn tail(bytes: &[u8], n: usize) -> String {
    let s = String::from_utf8_lossy(bytes);
    let s = s.trim();
    let start = s.char_indices().rev().nth(n).map(|(i, _)| i).unwrap_or(0);
    s[start..].to_string()
}

/// Turn captured sandbox output into an [`Outcome`]. Missing or malformed
/// harness output is an error, never a pass.
pub(crate) fn parse_outcome(c: &Captured, nonce: &str) -> Outcome {
    if c.aborted {
        return Outcome::failed("cancelled");
    }
    if c.timed_out {
        return Outcome {
            timed_out: true,
            ..Default::default()
        };
    }
    let stdout = String::from_utf8_lossy(&c.stdout);
    let prefix = format!("ACO-RESULT {nonce} ");
    let Some(line) = stdout.lines().find_map(|l| l.strip_prefix(prefix.as_str())) else {
        let code = c
            .status
            .map(|s| s.to_string())
            .unwrap_or_else(|| "unknown status".into());
        return Outcome::failed(format!(
            "sandbox produced no result ({code}): {}",
            tail(&c.stderr, 400)
        ));
    };
    let r: HarnessResult = match serde_json::from_str(line) {
        Ok(r) => r,
        Err(e) => return Outcome::failed(format!("unreadable harness result: {e}")),
    };
    let median_ns = crate::score::median(&r.times_ns);
    Outcome {
        stdout: r.stdout,
        stdout_truncated: r.stdout_truncated,
        stdout_sha256: r.stdout_sha256,
        median_ns,
        times_ns: r.times_ns,
        peak_rss_kb: r.cgroup_peak_bytes.map(|b| b / 1024).unwrap_or(r.maxrss_kb),
        timed_out: false,
        error: r.error,
    }
}
