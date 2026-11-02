//! `abort_all` (what Ctrl-C calls) kills in-flight sandbox runs promptly.
//! Its own test binary because the abort flag is process-wide.

use std::time::{Duration, Instant};

use aco::runner::{abort_all, select, Limits, SandboxKind};

#[test]
fn abort_kills_a_running_sandbox_quickly() {
    let r = match select(
        SandboxKind::Auto,
        Limits {
            timeout: Duration::from_secs(30),
            ..Limits::default()
        },
    ) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("SKIP abort test: {e}");
            return;
        }
    };
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_millis(1500));
        abort_all();
    });
    let t = Instant::now();
    let o = r.run("while True:\n    pass\n", 1);
    let took = t.elapsed();
    assert!(!o.ok());
    assert_eq!(o.error.as_deref(), Some("cancelled"), "{o:?}");
    assert!(took < Duration::from_secs(10), "abort took {took:?}");
    // And nothing new starts after an abort.
    let o = r.run("print(1)\n", 1);
    assert!(!o.ok());
}
