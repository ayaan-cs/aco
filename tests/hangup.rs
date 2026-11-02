//! Closing the terminal must end the dashboard. crossterm's input reader spins
//! at 100% CPU forever once its tty hangs up, so this drives the real binary in
//! a pseudo-terminal (via Python's `pty` module), closes the terminal while the
//! dashboard waits for `q`, and requires a prompt exit. It covers both a
//! controlling terminal (which also sends SIGHUP) and a terminal that is only
//! stdin/stdout (no SIGHUP; the hang-up must be noticed from I/O).

use std::process::Command;

const DRIVER: &str = r#"
import os, pty, select, signal, struct, subprocess, sys, time, fcntl, termios
binary, target, out, ctty = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4] == "1"
argv = [binary, "--sandbox", "auto", "-o", out, target]
env = dict(os.environ, TERM="xterm-256color")
# stderr goes to a file so a failure message reaches the test log.
err = open(out + ".stderr", "w")
master, slave = os.openpty()
fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 120, 0, 0))
def make_ctty():
    os.setsid()
    fcntl.ioctl(0, termios.TIOCSCTTY, 0)
p = subprocess.Popen(argv, stdin=slave, stdout=slave, stderr=err, env=env,
                     preexec_fn=make_ctty if ctty else None,
                     start_new_session=not ctty)
os.close(slave)
start = time.time()
seen = b""
# Wait until the run has finished and the dashboard waits for a key.
while time.time() - start < 60 and b"q quit" not in seen:
    r, _, _ = select.select([master], [], [], 0.2)
    if r:
        try:
            seen = (seen + os.read(master, 65536))[-200000:]
        except OSError:
            break
done = b"q quit" in seen
os.close(master)
closed = time.time()
try:
    code = p.wait(timeout=5)
    print("finished=%s exit=%d after=%.2f" % (done, code, time.time() - closed))
except subprocess.TimeoutExpired:
    p.kill()
    p.wait()
    print("finished=%s STILL RUNNING 5s after the terminal closed" % done)
    sys.exit(1)
"#;

fn run(controlling: bool) {
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
        if controlling { "ctty" } else { "plain" }
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
        .arg(if controlling { "1" } else { "0" })
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
fn closing_the_controlling_terminal_ends_the_dashboard() {
    run(true);
}

#[test]
fn closing_a_terminal_that_sends_no_sighup_ends_the_dashboard() {
    run(false);
}
