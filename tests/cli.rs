//! The binary's user-facing behaviour: exit codes, diagnostics, colour and
//! the JSON report. Cases that need a sandbox skip when none is available.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use aco::runner::{select, Limits, SandboxKind};

fn aco() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_aco"));
    c.env_remove("NO_COLOR").env("TERM", "xterm-256color");
    c
}

fn tmp(name: &str, contents: &[u8]) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("cli");
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(name);
    std::fs::write(&p, contents).unwrap();
    p
}

fn text(o: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

fn sandbox_available() -> bool {
    match select(SandboxKind::Auto, Limits::default()) {
        Ok(_) => true,
        Err(e) => {
            eprintln!("SKIP sandboxed CLI test: {e}");
            false
        }
    }
}

#[test]
fn help_has_sections_and_examples() {
    let o = aco().arg("--help").output().unwrap();
    let (out, _) = text(&o);
    assert!(o.status.success());
    for needle in [
        "Search:",
        "Proposer:",
        "Sandbox:",
        "Output:",
        "Examples:",
        "aco --plain fixtures/*.py",
        "Exit status:",
        "NO_COLOR",
    ] {
        assert!(out.contains(needle), "missing {needle:?}");
    }
    assert!(!out.contains('\x1b'), "help is coloured when piped");
}

#[test]
fn missing_file_is_a_clear_error() {
    let o = aco().arg("does-not-exist.py").output().unwrap();
    let (out, err) = text(&o);
    assert_eq!(o.status.code(), Some(2));
    assert!(out.is_empty());
    assert!(
        err.starts_with("error: `does-not-exist.py` does not exist"),
        "{err}"
    );
    assert!(err.contains("hint:"));
    assert!(!err.contains('\x1b'));
}

#[test]
fn syntax_error_points_at_the_line() {
    let p = tmp("bad.py", b"x = 1\ndef f(:\n    pass\n");
    let o = aco().arg(&p).output().unwrap();
    let (_, err) = text(&o);
    assert_eq!(o.status.code(), Some(2));
    assert!(err.contains("is not valid Python"), "{err}");
    assert!(err.contains("bad.py:2:"), "{err}");
    assert!(err.contains("2 | def f(:"), "{err}");
    assert!(err.contains('^'));
}

#[test]
fn empty_and_comment_only_files_are_refused() {
    for (name, body) in [("empty.py", &b""[..]), ("comments.py", b"# nothing\n\n")] {
        let p = tmp(name, body);
        let o = aco().arg(&p).output().unwrap();
        let (_, err) = text(&o);
        assert_eq!(o.status.code(), Some(2), "{name}");
        assert!(err.contains("has no code to optimise"), "{err}");
    }
}

#[test]
fn non_python_and_binary_files_are_refused() {
    let md = tmp("notes.md", b"# Title\n\nSome `code` here.\n");
    let o = aco().arg(&md).output().unwrap();
    let (_, err) = text(&o);
    assert_eq!(o.status.code(), Some(2));
    assert!(err.contains("is not valid Python"), "{err}");
    assert!(
        err.contains("aco optimises Python source files (.py)"),
        "{err}"
    );

    let bin = tmp("blob.py", &[0x7f, b'E', b'L', b'F', 0xff, 0xfe, 0x00, 0x01]);
    let o = aco().arg(&bin).output().unwrap();
    let (_, err) = text(&o);
    assert_eq!(o.status.code(), Some(2));
    assert!(err.contains("is not UTF-8 text"), "{err}");

    let o = aco().arg(env!("CARGO_MANIFEST_DIR")).output().unwrap();
    assert_eq!(o.status.code(), Some(2));
    assert!(text(&o).1.contains("is a directory"));
}

#[test]
fn policy_violations_never_reach_a_sandbox() {
    for (name, src, needle) in [
        (
            "uses_os.py",
            "import os\nprint(os.getcwd())\n",
            "banned module `os`",
        ),
        (
            "uses_subprocess.py",
            "from subprocess import run\nrun(['id'])\n",
            "banned module `subprocess`",
        ),
        (
            "uses_open.py",
            "print(open('/etc/passwd').read())\n",
            "banned builtin `open`",
        ),
        (
            "uses_eval.py",
            "print(eval('1+1'))\n",
            "banned builtin `eval`",
        ),
        (
            "uses_dunder.py",
            "print(().__class__)\n",
            "dunder attribute",
        ),
        ("uses_import.py", "m = __import__('os')\n", "`__import__`"),
    ] {
        let p = tmp(name, src.as_bytes());
        // --sandbox docker would fail with exit 3 if it got that far; exit 2
        // proves the policy stopped it before any sandbox was selected.
        let o = aco()
            .args(["--sandbox", "docker"])
            .arg(&p)
            .output()
            .unwrap();
        let (_, err) = text(&o);
        assert_eq!(o.status.code(), Some(2), "{name}: {err}");
        assert!(err.contains("rejected before running"), "{name}: {err}");
        assert!(err.contains(needle), "{name}: {err}");
    }
}

#[test]
fn usage_errors_exit_2() {
    for args in [
        &["--pop", "1", "x.py"][..],
        &["--reps", "4", "x.py"],
        &["--gens", "0", "x.py"],
        &["--margin", "1.5", "x.py"],
        &["--timeout", "0", "x.py"],
        &["--sandbox", "vm", "x.py"],
        &[],
    ] {
        let o = aco().args(args).output().unwrap();
        assert_eq!(o.status.code(), Some(2), "{args:?}");
    }
}

#[test]
fn colour_can_be_forced_or_disabled() {
    let o = aco()
        .args(["--color", "always", "nope.py"])
        .output()
        .unwrap();
    assert!(text(&o).1.contains("\x1b[1;31merror\x1b[0m"));
    let o = aco()
        .args(["--color", "never", "nope.py"])
        .output()
        .unwrap();
    assert!(!text(&o).1.contains('\x1b'));
}

#[test]
fn plain_run_writes_outputs_and_a_versioned_json_report() {
    if !sandbox_available() {
        return;
    }
    let p = tmp(
        "tiny.py",
        b"total = 0\nfor i in range(1000):\n    total += i\nprint(total)\n",
    );
    let out = Path::new(env!("CARGO_TARGET_TMPDIR")).join("cli-out");
    let _ = std::fs::remove_dir_all(&out);
    let o = aco()
        .args(["--plain", "--json", "--reps", "3", "--out"])
        .arg(&out)
        .arg(&p)
        .output()
        .unwrap();
    let (stdout, err) = text(&o);
    assert!(o.status.success(), "{stdout}\n{err}");
    assert!(!stdout.contains('\x1b'), "colour in piped output");
    assert!(
        stdout.contains("no faster version found, original kept"),
        "{stdout}"
    );
    let best = std::fs::read_to_string(out.join("tiny.best.py")).unwrap();
    assert_eq!(best, std::fs::read_to_string(&p).unwrap());
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("tiny.report.json")).unwrap())
            .unwrap();
    assert_eq!(v["schema_version"], 1);
    for key in [
        "tool",
        "file",
        "sandbox",
        "proposer",
        "config",
        "baseline",
        "candidates",
        "generations",
        "stop_reason",
        "confirmation",
        "best",
        "best_source",
    ] {
        assert!(v.get(key).is_some(), "report lacks {key}");
    }
    assert_eq!(v["baseline"]["times_ns"].as_array().unwrap().len(), 3);
    assert_eq!(v["best"]["confirmed"], false);
}
