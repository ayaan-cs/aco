//! Third-party requirements: finding them, validating them, and installing
//! them (wheels only, cached) outside any timed run.
//!
//! Installation is the one step that uses the network. It runs before the
//! search, never inside a timed sandbox run. `pip` is told `--only-binary
//! :all:`, so no package's `setup.py` or build backend runs; installing a wheel
//! only unpacks files. Package code first runs later, inside the sandbox, when
//! your script imports it. The policy pre-filter checks your code, not the
//! packages' internals, so the sandbox is what contains them.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::runner::run_captured;

/// Where the derived Docker image keeps installed packages.
pub const DOCKER_DEPS_DIR: &str = "/opt/aco-deps";

/// Validated requirement specifiers and where they came from.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Requirements {
    /// e.g. "requirements.txt" or "pyproject.toml [project.dependencies]".
    pub source: String,
    pub specs: Vec<String>,
}

/// One installed distribution.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Package {
    pub name: String,
    pub version: String,
    /// Importable top-level names it provides.
    pub top_level: Vec<String>,
}

/// The result of installing requirements for one backend.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Installed {
    pub key: String,
    /// The interpreter the packages were installed for.
    pub python: String,
    pub packages: Vec<Package>,
    /// Host directory (unshare).
    #[serde(skip)]
    pub host_dir: Option<PathBuf>,
    /// Derived image tag (Docker).
    pub docker_image: Option<String>,
    /// Served from the cache rather than installed now.
    pub cached: bool,
}

impl Installed {
    pub fn top_level(&self) -> BTreeSet<String> {
        self.packages
            .iter()
            .flat_map(|p| p.top_level.iter().cloned())
            .collect()
    }
}

// ----------------------------------------------------------------- parsing

fn valid_name(s: &str) -> bool {
    let mut c = s.chars();
    matches!(c.next(), Some(ch) if ch.is_ascii_alphanumeric())
        && s.chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
}

/// PEP 508 marker: marker variables, comparison operators, `and`/`or`/
/// `in`/`not`, parentheses and quoted strings, nothing else.
fn valid_marker(m: &str) -> bool {
    const VARS: &[&str] = &[
        "python_version",
        "python_full_version",
        "os_name",
        "sys_platform",
        "platform_release",
        "platform_system",
        "platform_version",
        "platform_machine",
        "platform_python_implementation",
        "implementation_name",
        "implementation_version",
        "extra",
        "and",
        "or",
        "in",
        "not",
    ];
    let c: Vec<char> = m.chars().collect();
    let mut i = 0;
    while i < c.len() {
        let ch = c[i];
        if ch.is_whitespace() || ch == '(' || ch == ')' {
            i += 1;
        } else if ch == '"' || ch == '\'' {
            let Some(end) = c[i + 1..].iter().position(|x| *x == ch) else {
                return false;
            };
            let inner: String = c[i + 1..i + 1 + end].iter().collect();
            if !inner
                .chars()
                .all(|x| x.is_ascii_alphanumeric() || " .*+-_!".contains(x))
            {
                return false;
            }
            i += end + 2;
        } else if "<>=!~".contains(ch) {
            i += 1;
        } else if ch.is_ascii_alphabetic() || ch == '_' {
            let start = i;
            while i < c.len() && (c[i].is_ascii_alphanumeric() || c[i] == '_') {
                i += 1;
            }
            let word: String = c[start..i].iter().collect();
            if !VARS.contains(&word.as_str()) {
                return false;
            }
        } else {
            return false;
        }
    }
    true
}

/// Check one PEP 508 requirement without URLs or paths, e.g.
/// `numpy>=2,<3; python_version >= "3.10"`.
pub fn validate_spec(spec: &str) -> Result<String, String> {
    let spec = spec.trim();
    if spec.contains("://") || spec.contains(" @ ") || spec.contains('@') {
        return Err("direct URL references are not supported; name a package from PyPI".into());
    }
    if spec.starts_with(['.', '/', '~']) {
        return Err("local paths are not supported; name a package from PyPI".into());
    }
    let (req, marker) = match spec.split_once(';') {
        Some((r, m)) => (r.trim(), Some(m.trim())),
        None => (spec, None),
    };
    let name_end = req
        .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')))
        .unwrap_or(req.len());
    let name = &req[..name_end];
    if !valid_name(name) {
        return Err("does not start with a package name".into());
    }
    let mut rest = req[name_end..].trim_start();
    if let Some(r) = rest.strip_prefix('[') {
        let Some(close) = r.find(']') else {
            return Err("unclosed [extras]".into());
        };
        if !r[..close]
            .split(',')
            .all(|e| valid_name(e.trim()) || e.trim().is_empty())
        {
            return Err("invalid extras".into());
        }
        rest = r[close + 1..].trim_start();
    }
    let rest = rest.trim_start_matches('(').trim_end_matches(')');
    if !rest
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || " <>=!~,.*+".contains(c))
    {
        return Err("invalid version specifier".into());
    }
    if let Some(m) = marker {
        if !valid_marker(m) {
            return Err("invalid environment marker".into());
        }
    }
    Ok(spec.to_string())
}

/// Parse a requirements file. Options (`-r`, `-e`, `--index-url`, hashes),
/// URLs and paths are refused with the line number, because they could make
/// pip build or fetch something other than a wheel from PyPI.
pub fn parse_requirements_txt(text: &str, name: &str) -> Result<Vec<String>, String> {
    let mut specs = Vec::new();
    let joined = text.replace("\\\r\n", " ").replace("\\\n", " ");
    for (i, raw) in joined.lines().enumerate() {
        let line = match raw
            .find(" #")
            .or_else(|| raw.trim_start().starts_with('#').then_some(0))
        {
            Some(0) if raw.trim_start().starts_with('#') => "",
            Some(k) => &raw[..k],
            None => raw,
        }
        .trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('-') {
            return Err(format!(
                "{name}:{}: `{line}`: pip options are not supported (aco installs plain \
                 PyPI requirements as wheels only)",
                i + 1
            ));
        }
        specs.push(validate_spec(line).map_err(|e| format!("{name}:{}: `{line}`: {e}", i + 1))?);
    }
    Ok(specs)
}

/// `[project] dependencies` from a pyproject.toml.
pub fn parse_pyproject(text: &str, name: &str) -> Result<Option<Vec<String>>, String> {
    let v: toml::Value = toml::from_str(text).map_err(|e| format!("{name}: {e}"))?;
    let Some(deps) = v.get("project").and_then(|p| p.get("dependencies")) else {
        return Ok(None);
    };
    let arr = deps
        .as_array()
        .ok_or_else(|| format!("{name}: [project] dependencies is not a list"))?;
    let mut specs = Vec::new();
    for d in arr {
        let s = d
            .as_str()
            .ok_or_else(|| format!("{name}: a dependency is not a string"))?;
        specs.push(validate_spec(s).map_err(|e| format!("{name}: `{s}`: {e}"))?);
    }
    Ok(Some(specs))
}

/// `p` relative to the current directory when it is under it, for messages.
pub fn shown_path(p: &Path) -> String {
    let abs = p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    std::env::current_dir()
        .ok()
        .and_then(|c| c.canonicalize().ok())
        .and_then(|c| abs.strip_prefix(c).ok().map(|r| r.display().to_string()))
        .unwrap_or_else(|| p.display().to_string())
}

/// Requirements for a run: `--requirements FILE` if given, else
/// requirements.txt, else pyproject.toml `[project] dependencies`, looked up
/// in the project root. `Ok(None)` when there are none.
pub fn discover(explicit: Option<&Path>, root: &Path) -> Result<Option<Requirements>, String> {
    let read = |p: &Path| {
        std::fs::read_to_string(p).map_err(|e| format!("cannot read `{}`: {e}", p.display()))
    };
    if let Some(p) = explicit {
        let text = read(p)?;
        let shown = shown_path(p);
        let specs = if p.file_name().is_some_and(|n| n == "pyproject.toml") {
            parse_pyproject(&text, &shown)?.unwrap_or_default()
        } else {
            parse_requirements_txt(&text, &shown)?
        };
        return Ok(Some(Requirements {
            source: shown,
            specs,
        }));
    }
    let txt = root.join("requirements.txt");
    if txt.is_file() {
        let shown = shown_path(&txt);
        let specs = parse_requirements_txt(&read(&txt)?, &shown)?;
        return Ok((!specs.is_empty()).then_some(Requirements {
            source: shown,
            specs,
        }));
    }
    let py = root.join("pyproject.toml");
    if py.is_file() {
        let shown = shown_path(&py);
        if let Some(specs) = parse_pyproject(&read(&py)?, &shown)? {
            return Ok((!specs.is_empty()).then_some(Requirements {
                source: format!("{shown} [project] dependencies"),
                specs,
            }));
        }
    }
    Ok(None)
}

// ------------------------------------------------------------- installing

/// Lists installed distributions and their top-level import names. Reads
/// metadata files only; no package code is imported.
const META: &str = r#"
import json, sys
from importlib import metadata
out = []
for dist in metadata.distributions(path=[sys.argv[1]]):
    top = set()
    tl = dist.read_text("top_level.txt")
    if tl:
        top.update(x.strip() for x in tl.splitlines() if x.strip())
    for f in dist.files or []:
        parts = str(f).split("/")
        first = parts[0]
        if first.endswith((".dist-info", ".data", ".libs")) or first in ("..", "bin", "__pycache__"):
            continue
        if len(parts) == 1:
            if first.endswith(".py"):
                top.add(first[:-3])
            elif first.endswith(".so") or first.endswith(".pyd"):
                top.add(first.split(".")[0])
        else:
            top.add(first)
    out.append({"name": dist.metadata["Name"], "version": dist.version, "top_level": sorted(top)})
out.sort(key=lambda d: d["name"].lower())
print(json.dumps({"python": sys.version.split()[0], "packages": out}))
"#;

#[derive(Deserialize)]
struct Meta {
    python: String,
    packages: Vec<Package>,
}

fn sha256_hex(parts: &[&str]) -> String {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p.as_bytes());
        h.update([0u8]);
    }
    format!("{:x}", h.finalize())
}

fn cache_root() -> PathBuf {
    if let Some(x) = std::env::var_os("ACO_CACHE_DIR") {
        return PathBuf::from(x);
    }
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(std::env::temp_dir);
    base.join("aco")
}

/// Turn pip's failure output into a message that names the package.
pub fn explain_pip_failure(output: &str, python: &str) -> String {
    if output.contains("NewConnectionError")
        || output.contains("Failed to establish")
        || output.contains("Temporary failure in name resolution")
    {
        return format!(
            "pip could not reach the package index (installing requirements needs network \
             once; later runs use the cache):\n{}",
            tail(output, 4)
        );
    }
    const NO_MATCH: &str = "ERROR: No matching distribution found for ";
    for line in output.lines() {
        // Docker BuildKit prefixes build output with step numbers and times.
        if let Some(rest) = line.find(NO_MATCH).map(|i| &line[i + NO_MATCH.len()..]) {
            return format!(
                "`{}` has no installable wheel for this interpreter ({python}). aco installs \
                 wheels only (`--only-binary :all:`), so no package build script ever runs. Pin \
                 a version that publishes wheels for this Python and platform, or remove it.",
                rest.trim()
            );
        }
    }
    format!("pip failed:\n{}", tail(output, 8))
}

fn tail(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

fn requirements_file(reqs: &Requirements) -> String {
    let mut s = reqs.specs.join("\n");
    s.push('\n');
    s
}

/// Install for the unshare backend with the same interpreter the sandbox runs,
/// into a cached directory keyed by interpreter and requirements.
pub fn install_host(python: &Path, reqs: &Requirements) -> Result<Installed, String> {
    let probe = Command::new(python)
        .args([
            "-I",
            "-c",
            "import sys,sysconfig;print(sys.version.split()[0], sys.implementation.cache_tag, sysconfig.get_platform())",
        ])
        .output()
        .map_err(|e| format!("cannot run {}: {e}", python.display()))?;
    let interp = String::from_utf8_lossy(&probe.stdout).trim().to_string();
    let req_text = requirements_file(reqs);
    let key = sha256_hex(&[
        "aco-deps-host-v1",
        &python.to_string_lossy(),
        &interp,
        &req_text,
    ]);
    let dir = cache_root().join("deps").join(&key[..24]);
    let meta_path = dir.join("meta.json");
    let site = dir.join("site-packages");
    let load = |cached: bool| -> Result<Installed, String> {
        let text = std::fs::read_to_string(&meta_path).map_err(|e| e.to_string())?;
        let m: Meta = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        Ok(Installed {
            key: key.clone(),
            python: m.python,
            packages: m.packages,
            host_dir: Some(site.clone()),
            docker_image: None,
            cached,
        })
    };
    if meta_path.is_file() {
        if let Ok(i) = load(true) {
            return Ok(i);
        }
    }
    let tmp = dir.with_extension(format!("tmp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).map_err(|e| format!("cannot create {}: {e}", tmp.display()))?;
    let req_path = tmp.join("requirements.txt");
    std::fs::write(&req_path, &req_text).map_err(|e| e.to_string())?;
    let mut cmd = Command::new(python);
    cmd.args(["-I", "-m", "pip", "install"])
        .args([
            "--only-binary=:all:",
            "--isolated",
            "--disable-pip-version-check",
            "--no-input",
            "--no-compile",
            "--quiet",
            "--target",
        ])
        .arg(tmp.join("site-packages"))
        .arg("-r")
        .arg(&req_path);
    let c = run_captured(cmd, Duration::from_secs(900), 1 << 20, &|| {})
        .map_err(|e| format!("cannot run pip: {e}"))?;
    let out = format!(
        "{}{}",
        String::from_utf8_lossy(&c.stdout),
        String::from_utf8_lossy(&c.stderr)
    );
    if c.timed_out || !c.status.is_some_and(|s| s.success()) {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(explain_pip_failure(&out, &interp));
    }
    let m = Command::new(python)
        .args(["-I", "-c", META])
        .arg(tmp.join("site-packages"))
        .output()
        .map_err(|e| e.to_string())?;
    if !m.status.success() {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(format!(
            "could not read installed package metadata: {}",
            String::from_utf8_lossy(&m.stderr)
        ));
    }
    std::fs::write(tmp.join("meta.json"), &m.stdout).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::rename(&tmp, &dir).map_err(|e| format!("cannot finish {}: {e}", dir.display()))?;
    load(false)
}

/// Build (or reuse) a derived image `FROM` the pinned base with the
/// requirements installed. The build has network; runs never do.
pub fn install_docker(bin: &Path, base: &str, reqs: &Requirements) -> Result<Installed, String> {
    let req_text = requirements_file(reqs);
    let key = sha256_hex(&["aco-deps-docker-v1", base, &req_text]);
    let tag = format!("aco-deps:{}", &key[..24]);
    let quick = |args: &[&str], secs: u64| {
        let mut cmd = Command::new(bin);
        cmd.args(args);
        run_captured(cmd, Duration::from_secs(secs), 1 << 20, &|| {})
            .map_err(|e| format!("cannot run docker: {e}"))
    };
    let exists = quick(&["image", "inspect", &tag], 30)?
        .status
        .is_some_and(|s| s.success());
    if !exists {
        let ctx = std::env::temp_dir().join(format!("aco-build-{}", &key[..16]));
        let _ = std::fs::remove_dir_all(&ctx);
        std::fs::create_dir_all(&ctx).map_err(|e| e.to_string())?;
        std::fs::write(ctx.join("requirements.txt"), &req_text).map_err(|e| e.to_string())?;
        std::fs::write(
            ctx.join("Dockerfile"),
            format!(
                "FROM {base}\nCOPY requirements.txt /tmp/aco-requirements.txt\n\
                 RUN python3 -m pip install --only-binary=:all: --isolated \
                 --disable-pip-version-check --no-input --no-cache-dir --no-compile --quiet \
                 --target {DOCKER_DEPS_DIR} -r /tmp/aco-requirements.txt \
                 && chmod -R a+rX {DOCKER_DEPS_DIR}\n"
            ),
        )
        .map_err(|e| e.to_string())?;
        let ctx_s = ctx.to_string_lossy().to_string();
        let label = format!("aco.deps.key={key}");
        let c = quick(&["build", "--label", &label, "-t", &tag, &ctx_s], 1800)?;
        let _ = std::fs::remove_dir_all(&ctx);
        if c.timed_out || !c.status.is_some_and(|s| s.success()) {
            let out = format!(
                "{}{}",
                String::from_utf8_lossy(&c.stdout),
                String::from_utf8_lossy(&c.stderr)
            );
            return Err(explain_pip_failure(&out, "the Docker image's Python"));
        }
    }
    let c = quick(
        &[
            "run",
            "--rm",
            "--network",
            "none",
            "--read-only",
            &tag,
            "python3",
            "-I",
            "-c",
            META,
            DOCKER_DEPS_DIR,
        ],
        120,
    )?;
    if !c.status.is_some_and(|s| s.success()) {
        return Err(format!(
            "could not read installed package metadata: {}",
            String::from_utf8_lossy(&c.stderr)
        ));
    }
    let m: Meta = serde_json::from_slice(&c.stdout).map_err(|e| e.to_string())?;
    Ok(Installed {
        key,
        python: m.python,
        packages: m.packages,
        host_dir: None,
        docker_image: Some(tag),
        cached: exists,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_requirements_parse() {
        let t = "# deps\nnumpy==2.1.3  # pinned\n\nrequests[socks]>=2,<3\nattrs ; python_version >= \"3.8\"\n";
        assert_eq!(
            parse_requirements_txt(t, "r.txt").unwrap(),
            [
                "numpy==2.1.3",
                "requests[socks]>=2,<3",
                "attrs ; python_version >= \"3.8\""
            ]
        );
    }

    #[test]
    fn options_urls_and_paths_are_refused_with_the_line() {
        for (t, needle) in [
            ("numpy\n-e .\n", "r.txt:2"),
            ("--index-url https://x\n", "pip options"),
            ("pkg @ https://x/y.whl\n", "direct URL"),
            ("./local\n", "local paths"),
            ("numpy --hash=sha256:abc\n", "invalid version"),
            ("numpy; os.system('x')\n", "invalid environment marker"),
        ] {
            let e = parse_requirements_txt(t, "r.txt").unwrap_err();
            assert!(e.contains(needle), "{t:?}: {e}");
        }
    }

    #[test]
    fn pyproject_dependencies_parse() {
        let t = "[project]\nname = \"x\"\ndependencies = [\"numpy>=2\", \"attrs\"]\n";
        assert_eq!(
            parse_pyproject(t, "p").unwrap().unwrap(),
            ["numpy>=2", "attrs"]
        );
        assert_eq!(parse_pyproject("[tool.x]\n", "p").unwrap(), None);
    }

    #[test]
    fn pip_failure_names_the_package() {
        let out = "ERROR: Could not find a version that satisfies the requirement pycairo==1.20.0 (from versions: none)\nERROR: No matching distribution found for pycairo==1.20.0\n";
        let e = explain_pip_failure(out, "3.13.5");
        assert!(
            e.starts_with("`pycairo==1.20.0` has no installable wheel"),
            "{e}"
        );
    }
}
