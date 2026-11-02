//! Closing the terminal must end the dashboard. crossterm's input reader spins
//! at 100% CPU forever once its tty hangs up, so this drives the real binary in
//! a pseudo-terminal (via Python's `pty` module), closes the terminal while the
//! dashboard waits for `q`, and requires a prompt exit. It also covers a
//! process that ignores SIGHUP, which must notice the hang-up by itself.

use std::process::Command;

const DRIVER: &str = r#"
import os, pty, select, signal, struct, sys, time, fcntl, termios
binary, target, out, ignore = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4] == "1"
pid, fd = pty.fork()
if pid == 0:
    os.environ["TERM"] = "xterm-256color"
    # Keep stderr off the tty so a failure message reaches the test log.
    err = os.open(out + ".stderr", os.O_WRONLY | os.O_CREAT | os.O_TRUNC)
    os.dup2(err, 2)
    if ignore:
        signal.signal(signal.SIGHUP, signal.SIG_IGN)
    os.execv(binary, [binary, "--sandbox", "auto", "-o", out, target])
fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 120, 0, 0))
start = time.time()
seen = b""
# Wait until the run has finished and the dashboard waits for a key.
while time.time() - start < 60 and b"q quit" not in seen:
    r, _, _ = select.select([fd], [], [], 0.2)
    if r:
        try:
            seen = (seen + os.read(fd, 65536))[-200000:]
        except OSError:
            break
done = b"q quit" in seen
os.close(fd)
closed = time.time()
while time.time() - closed < 5:
    p, status = os.waitpid(pid, os.WNOHANG)
    if p:
        print("finished=%s exit=%d after=%.2f" % (done, os.waitstatus_to_exitcode(status), time.time() - closed))
        sys.exit(0)
    time.sleep(0.05)
os.kill(pid, signal.SIGKILL)
os.waitpid(pid, 0)
print("finished=%s STILL RUNNING 5s after the terminal closed" % done)
sys.exit(1)
"#;

fn run(ignore_sighup: bool) {
    if aco::runner::select(
        aco::runner::SandboxKind::Auto,
        aco::runner::Limits::default(),
    )
    .is_err()
    {
        eprintln!("skipping: no sandbox available");
        return;
    }
    let tmp = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "hangup-{}",
        if ignore_sighup { "ign" } else { "hup" }
    ));
    let out = Command::new("python3")
        .arg("-c")
        .arg(DRIVER)
        .arg(env!("CARGO_BIN_EXE_aco"))
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/fixtures/stress/tiny.py"
        ))
        .arg(&tmp)
        .arg(if ignore_sighup { "1" } else { "0" })
        .output()
        .expect("python3 runs");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = std::fs::read_to_string(tmp.with_extension("stderr")).unwrap_or_default();
    eprintln!("{stdout}{stderr}");
    assert!(
        out.status.success(),
        "aco kept running after the terminal closed: {stdout} {stderr}"
    );
    assert!(
        stdout.contains("finished=True"),
        "the dashboard never finished: {stdout}"
    );
    // Cancelled by the hang-up (130), not a panic or abort.
    assert!(
        stdout.contains("exit=130"),
        "unexpected exit: {stdout} {stderr}"
    );
    assert!(!stderr.contains("panicked"), "{stderr}");
}

#[test]
fn closing_the_terminal_ends_the_dashboard() {
    run(false);
}

#[test]
fn closing_the_terminal_ends_the_dashboard_even_if_sighup_is_ignored() {
    run(true);
}
