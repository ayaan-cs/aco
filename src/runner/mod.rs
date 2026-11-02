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
    /// Captured stderr (Python-level writes, warnings) of the first timed rep.
    pub stderr: String,
    /// SHA-256 of the full stderr.
    pub stderr_sha256: String,
    /// Exit status: 0, or the code of a `SystemExit` the script raised.
    pub exit_code: i64,
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
    /// Like [`Runner::run`] with `PYTHONHASHSEED` fixed to `seed`. Runners
    /// that cannot vary it ignore the seed.
    fn run_seeded(&self, source: &str, reps: usize, seed: u32) -> Outcome {
        let _ = seed;
        self.run(source, reps)
    }
}

/// Installed third-party requirements, as each backend sees them.
#[derive(Clone, Debug, Default)]
pub struct DepsEnv {
    /// Host directory with the installed packages (unshare mounts it at /deps).
    pub host_dir: Option<PathBuf>,
    /// Derived Docker image with the packages under /opt/aco-deps.
    pub docker_image: Option<String>,
}

/// Where and how a candidate runs.
#[derive(Clone, Debug, Default)]
pub struct RunCtx {
    /// Snapshot directory of the project's local modules, mounted read-only
    /// at /proj. `None` runs the candidate on its own.
    pub project: Option<PathBuf>,
    /// The entry point, relative to the project root.
    pub target: String,
    /// The file the candidate replaces, relative to the project root.
    pub mutated: String,
    /// `sys.argv[1:]` for the script.
    pub argv: Vec<String>,
    /// Text the script reads from stdin (empty if `None`).
    pub stdin: Option<String>,
    pub deps: DepsEnv,
}

/// A sandbox implementation. Wrap it in [`Bound`] to get a [`Runner`].
pub trait Backend: Send + Sync {
    fn name(&self) -> &'static str;
    fn exec(&self, ctx: &RunCtx, source: &str, reps: usize, seed: u32) -> Outcome;
    /// Install `reqs` (already sanitised) for this backend, cached. Network is
    /// used here, never during a timed run.
    fn prepare_deps(
        &self,
        reqs: &crate::deps::Requirements,
    ) -> Result<crate::deps::Installed, String>;
}

/// A backend plus the context for one input file.
pub struct Bound<'a> {
    pub backend: &'a dyn Backend,
    pub ctx: RunCtx,
}

impl<'a> Bound<'a> {
    /// Run candidates as standalone files.
    pub fn single(backend: &'a dyn Backend) -> Self {
        Bound {
            backend,
            ctx: RunCtx::default(),
        }
    }
}

impl Runner for Bound<'_> {
    fn name(&self) -> &'static str {
        self.backend.name()
    }
    fn run(&self, source: &str, reps: usize) -> Outcome {
        self.backend.exec(&self.ctx, source, reps, 0)
    }
    fn run_seeded(&self, source: &str, reps: usize, seed: u32) -> Outcome {
        self.backend.exec(&self.ctx, source, reps, seed)
    }
}

/// Environment for every sandboxed Python: hash seed fixed per run, and
/// numeric libraries kept to one thread so timings are comparable.
pub(crate) fn sandbox_env(seed: u32) -> Vec<(String, String)> {
    let mut v = vec![("PYTHONHASHSEED".to_string(), seed.to_string())];
    for k in [
        "OMP_NUM_THREADS",
        "OPENBLAS_NUM_THREADS",
        "MKL_NUM_THREADS",
        "NUMEXPR_NUM_THREADS",
    ] {
        v.push((k.to_string(), "1".to_string()));
    }
    v
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
pub fn select(kind: SandboxKind, limits: Limits) -> Result<Box<dyn Backend>, String> {
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
                    "no sandbox available, refusing to run candidates.\n  docker: {docker_err}\n  unshare: Linux-only (this is {}); on macOS, install and start Docker Desktop, OrbStack or colima so that `docker version` works",
                    std::env::consts::OS
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

/// A random hex string for unique scratch names.
/// Where per-run scratch directories go. They are bind-mounted into Docker
/// containers, so on macOS they must live somewhere the Docker VM shares with
/// the host: colima and Lima share only the home directory by default (not
/// `/var/folders`, which is where `$TMPDIR` points), so we use
/// `~/.cache/aco/run` there. `ACO_SCRATCH_DIR` overrides this everywhere.
pub fn scratch_root() -> std::path::PathBuf {
    if let Some(x) = std::env::var_os("ACO_SCRATCH_DIR") {
        return std::path::PathBuf::from(x);
    }
    if cfg!(target_os = "linux") {
        return std::env::temp_dir();
    }
    match std::env::var_os("HOME") {
        Some(h) if !h.is_empty() => std::path::PathBuf::from(h).join(".cache/aco/run"),
        _ => std::env::temp_dir(),
    }
}

pub fn nonce_public() -> String {
    nonce()
}

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

/// A private scratch directory holding `files/harness.py`,
/// `files/candidate.py`, `files/run.json` and an empty `root/` mount point.
/// Removed on drop.
pub(crate) struct WorkDir {
    pub dir: PathBuf,
}

/// Inside the sandbox: harness and run file at /work, project at /proj.
pub(crate) const WORK: &str = "/work";
pub(crate) const PROJ: &str = "/proj";

impl WorkDir {
    pub fn new(
        source: &str,
        ctx: &RunCtx,
        reps: usize,
        warmup: usize,
        nonce: &str,
        deps_mount: Option<&str>,
    ) -> std::io::Result<Self> {
        let dir = scratch_root().join(format!("aco-{}-{}", std::process::id(), self::nonce()));
        std::fs::create_dir_all(dir.join("files"))?;
        std::fs::create_dir_all(dir.join("root"))?;
        std::fs::write(dir.join("files/harness.py"), HARNESS)?;
        std::fs::write(dir.join("files/candidate.py"), source)?;
        let target = if ctx.project.is_some() {
            format!("{PROJ}/{}", ctx.target)
        } else {
            format!("{PROJ}/candidate.py")
        };
        let run = serde_json::json!({
            "target": target,
            "proj": PROJ,
            "reps": reps,
            "warmup": warmup,
            "nonce": nonce,
            "argv": ctx.argv,
            "stdin": ctx.stdin,
            "deps": deps_mount,
        });
        std::fs::write(dir.join("files/run.json"), run.to_string())?;
        // Readable by the sandbox user (Docker runs as nobody).
        use std::os::unix::fs::PermissionsExt;
        for f in [
            "files",
            "files/harness.py",
            "files/candidate.py",
            "files/run.json",
        ] {
            let mode = if f == "files" { 0o755 } else { 0o644 };
            std::fs::set_permissions(dir.join(f), std::fs::Permissions::from_mode(mode))?;
        }
        Ok(WorkDir { dir })
    }
    /// The project directory to mount at /proj: the snapshot, or the work
    /// files themselves for a standalone candidate.
    pub fn project(&self, ctx: &RunCtx) -> PathBuf {
        ctx.project.clone().unwrap_or_else(|| self.files())
    }
    /// The project-relative path the candidate is mounted over, if any.
    pub fn overlay<'c>(&self, ctx: &'c RunCtx) -> Option<&'c str> {
        ctx.project.as_ref().map(|_| ctx.mutated.as_str())
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
    #[serde(default)]
    stderr: String,
    #[serde(default)]
    stderr_sha256: String,
    #[serde(default)]
    exit_code: i64,
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
        stderr: r.stderr,
        stderr_sha256: r.stderr_sha256,
        exit_code: r.exit_code,
        median_ns,
        times_ns: r.times_ns,
        peak_rss_kb: r.cgroup_peak_bytes.map(|b| b / 1024).unwrap_or(r.maxrss_kb),
        timed_out: false,
        error: r.error,
    }
}
