//! Linux fallback sandbox built from unprivileged user namespaces.
//!
//! `unshare --user --map-root-user --mount --pid --fork --mount-proc --net`
//! gives the candidate its own user, mount, PID and network namespaces (the
//! network namespace has only a downed loopback). Inside, a constant setup
//! script builds a private tmpfs root holding read-only binds of `/usr` and of
//! the work files and the project snapshot (plus the candidate mounted over
//! the file it replaces, and installed requirements at /deps), `pivot_root`s
//! into it and detaches the host root, then drops
//! every capability with `setpriv --no-new-privs`, clears the environment and
//! applies rlimits with `prlimit` before exec'ing the harness.
//!
//! If user namespaces are unavailable this runner refuses to run.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use super::{parse_outcome, run_captured, Backend, Limits, Outcome, RunCtx, Runner, WorkDir};
use crate::deps::{Installed, Requirements};

/// Runs inside the new namespaces. Positional arguments only; nothing from the
/// candidate is interpolated into the script.
const SETUP: &str = r#"set -eu
root="$1"; files="$2"; proj="$3"; overlay="$4"; deps="$5"; seed="$6"
cpu="$7"; as="$8"; fsize="$9"; nproc="${10}"; nofile="${11}"; py="${12}"
shift 12
mount -t tmpfs -o size=8m,mode=755 aco-root "$root"
mkdir -p "$root/usr" "$root/work" "$root/proj" "$root/tmp" "$root/proc" "$root/dev" "$root/.old"
mount --rbind /usr "$root/usr"
mount -o remount,bind,ro "$root/usr"
ln -s usr/bin "$root/bin"
ln -s usr/lib "$root/lib"
if [ -d /usr/lib64 ]; then ln -s usr/lib64 "$root/lib64"; fi
mount --bind "$files" "$root/work"
mount -o remount,bind,ro "$root/work"
mount --bind "$proj" "$root/proj"
mount -o remount,bind,ro "$root/proj"
if [ "$overlay" != "-" ]; then
  mount --bind "$files/candidate.py" "$root/proj/$overlay"
  mount -o remount,bind,ro "$root/proj/$overlay"
fi
if [ "$deps" != "-" ]; then
  mkdir -p "$root/deps"
  mount --bind "$deps" "$root/deps"
  mount -o remount,bind,ro "$root/deps"
fi
mount -t tmpfs -o size=64m,nosuid,nodev tmpfs "$root/tmp"
touch "$root/dev/null"
mount --bind /dev/null "$root/dev/null"
mount -t proc -o nosuid,nodev,noexec proc "$root/proc"
cd "$root"
pivot_root . .old
umount -l /.old
rmdir /.old
mount -o remount,bind,ro /
cd /tmp
exec setpriv --no-new-privs --inh-caps=-all --ambient-caps=-all --bounding-set=-all -- \
  /usr/bin/env -i PATH=/usr/bin HOME=/tmp PYTHONHASHSEED="$seed" OMP_NUM_THREADS=1 \
  OPENBLAS_NUM_THREADS=1 MKL_NUM_THREADS=1 NUMEXPR_NUM_THREADS=1 \
  /usr/bin/prlimit --cpu="$cpu" --as="$as" --fsize="$fsize" --nproc="$nproc" --nofile="$nofile" -- \
  "$py" -s -B "$@"
"#;

const NS_FLAGS: &[&str] = &[
    "--user",
    "--map-root-user",
    "--mount",
    "--pid",
    "--fork",
    "--mount-proc",
    "--net",
];

const SETUP_PATH: &str = "/usr/sbin:/usr/bin:/sbin:/bin";

pub struct UnshareRunner {
    pub unshare_bin: PathBuf,
    pub python: PathBuf,
    pub limits: Limits,
}

/// The fail-closed message when unprivileged user namespaces are off.
pub fn userns_unavailable(detail: &str) -> String {
    format!(
        "unprivileged user namespaces are unavailable ({detail}). aco will not run \
         generated code without isolation. Check `sysctl kernel.unprivileged_userns_clone` \
         (Debian-style kernels need 1) and `sysctl kernel.apparmor_restrict_unprivileged_userns` \
         (Ubuntu 23.10+ sets 1, which blocks this; an AppArmor profile for unshare is the \
         targeted fix). Changing either setting is your call; aco never changes them. \
         Or install Docker and use --sandbox docker."
    )
}

fn find_tool(name: &str) -> Option<PathBuf> {
    ["/usr/bin", "/usr/sbin", "/bin", "/sbin"]
        .iter()
        .map(|d| Path::new(d).join(name))
        .find(|p| p.exists())
}

impl UnshareRunner {
    pub fn new(limits: Limits) -> Result<Self, String> {
        let unshare_bin =
            find_tool("unshare").ok_or("the `unshare` binary (util-linux) was not found")?;
        let python = ["/usr/bin/python3", "/usr/local/bin/python3"]
            .iter()
            .map(PathBuf::from)
            .find(|p| p.exists())
            .ok_or("python3 was not found under /usr (only /usr is visible inside the sandbox)")?;
        Ok(UnshareRunner {
            unshare_bin,
            python,
            limits,
        })
    }

    /// Check that namespaces can be created and the setup tools exist.
    pub fn probe(&self) -> Result<(), String> {
        let mut cmd = Command::new(&self.unshare_bin);
        cmd.args(NS_FLAGS)
            .arg("true")
            .env_clear()
            .env("PATH", SETUP_PATH);
        let c = run_captured(cmd, Duration::from_secs(10), 64 << 10, &|| {})
            .map_err(|e| userns_unavailable(&format!("could not start unshare: {e}")))?;
        let ok = c.status.map(|s| s.success()).unwrap_or(false);
        if !ok {
            let msg = String::from_utf8_lossy(&c.stderr).trim().to_string();
            let msg = if msg.is_empty() {
                "unshare exited with an error".to_string()
            } else {
                msg
            };
            return Err(userns_unavailable(&msg));
        }
        for tool in ["setpriv", "prlimit", "pivot_root", "mount", "umount"] {
            if find_tool(tool).is_none() {
                return Err(format!(
                    "`{tool}` (util-linux) is required by the unshare sandbox"
                ));
            }
        }
        Ok(())
    }
}

impl Backend for UnshareRunner {
    fn name(&self) -> &'static str {
        "unshare"
    }

    fn exec(&self, ctx: &RunCtx, source: &str, reps: usize, seed: u32) -> Outcome {
        let l = &self.limits;
        let nonce = super::nonce();
        let deps = ctx.deps.host_dir.as_ref();
        let wd = match WorkDir::new(source, ctx, reps, l.warmup, &nonce, deps.map(|_| "/deps")) {
            Ok(w) => w,
            Err(e) => return Outcome::failed(format!("could not create work dir: {e}")),
        };
        let mut cmd = Command::new(&self.unshare_bin);
        cmd.args(NS_FLAGS)
            .arg("--kill-child")
            .arg("--")
            .arg("/bin/sh")
            .arg("-c")
            .arg(SETUP)
            .arg("aco-sandbox")
            .arg(wd.root())
            .arg(wd.files())
            .arg(wd.project(ctx))
            .arg(wd.overlay(ctx).unwrap_or("-"))
            .arg(
                deps.map(|d| d.as_os_str().to_owned())
                    .unwrap_or_else(|| "-".into()),
            )
            .arg(seed.to_string())
            .arg(l.cpu_secs().to_string())
            .arg((l.memory_mb << 20).to_string())
            .arg((l.fsize_mb << 20).to_string())
            .arg(l.pids.to_string())
            .arg(l.nofile.to_string())
            .arg(&self.python)
            .arg("/work/harness.py")
            .arg("/work/run.json")
            .env_clear()
            .env("PATH", SETUP_PATH);
        match run_captured(cmd, l.timeout, l.output_cap, &|| {}) {
            Ok(c) => parse_outcome(&c, &nonce),
            Err(e) => Outcome::failed(format!("could not start unshare: {e}")),
        }
    }

    fn prepare_deps(&self, reqs: &Requirements) -> Result<Installed, String> {
        crate::deps::install_host(&self.python, reqs)
    }
}

impl Runner for UnshareRunner {
    fn name(&self) -> &'static str {
        "unshare"
    }
    fn run(&self, source: &str, reps: usize) -> Outcome {
        self.exec(&RunCtx::default(), source, reps, 0)
    }
    fn run_seeded(&self, source: &str, reps: usize, seed: u32) -> Outcome {
        self.exec(&RunCtx::default(), source, reps, seed)
    }
}
