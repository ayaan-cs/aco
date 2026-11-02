//! Isolation tests. Each one fails if the isolation it checks is removed.
//! Docker cases skip with a message when no daemon is reachable; unshare cases
//! skip with the probe's reason when user namespaces are unavailable.

use std::time::Duration;

use aco::runner::unshare::userns_unavailable;
use aco::runner::{DockerRunner, Limits, Runner, UnshareRunner};
use aco::score::{judge, Decision, Oracle};

const SENTINEL: &str = "ACO_SENTINEL_9F3A";

fn limits() -> Limits {
    Limits {
        timeout: Duration::from_secs(4),
        ..Limits::default()
    }
}

fn unshare() -> Option<UnshareRunner> {
    match UnshareRunner::new(limits()).and_then(|u| u.probe().map(|_| u)) {
        Ok(u) => Some(u),
        Err(e) => {
            eprintln!("SKIP unshare isolation test: {e}");
            None
        }
    }
}

fn docker() -> Option<DockerRunner> {
    let d = DockerRunner::new(limits());
    match d.probe() {
        Ok(()) => Some(d),
        Err(e) => {
            eprintln!("SKIP docker isolation test: {e}");
            None
        }
    }
}

fn env_is_clean(r: &dyn Runner) {
    // Safe in edition 2021; the value only has to exist in the parent.
    std::env::set_var(SENTINEL, "leaked");
    let o = r.run(
        &format!("import os\nprint(os.environ.get('{SENTINEL}', 'absent'))\n"),
        1,
    );
    assert!(o.ok(), "{o:?}");
    assert_eq!(
        o.stdout.trim(),
        "absent",
        "host env leaked into {}",
        r.name()
    );
}

fn host_file_is_unreadable(r: &dyn Runner) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/aco-host-secret.txt");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "host-secret-71c2").unwrap();
    // A container has its own /etc/hostname; what must not show up is the
    // host's file content. The unshare root has no /etc at all.
    let host_hostname = std::fs::read_to_string("/etc/hostname").unwrap_or_default();
    let src = format!(
        "try:\n    print(open({:?}).read())\nexcept OSError as e:\n    print('blocked', type(e).__name__)\n\
         try:\n    print(open('/etc/hostname').read())\nexcept OSError as e:\n    print('blocked', type(e).__name__)\n",
        path.to_string_lossy()
    );
    let o = r.run(&src, 1);
    assert!(o.ok(), "{o:?}");
    assert!(
        !o.stdout.contains("host-secret-71c2"),
        "host file readable in {}",
        r.name()
    );
    assert!(o.stdout.starts_with("blocked"), "{}", o.stdout);
    let host_hostname = host_hostname.trim();
    if !host_hostname.is_empty() {
        assert!(
            !o.stdout.contains(host_hostname),
            "host /etc/hostname visible in {}: {}",
            r.name(),
            o.stdout
        );
    }
}

fn network_is_unreachable(r: &dyn Runner) {
    let src = "import socket\n\
               for host in ('1.1.1.1', '8.8.8.8'):\n\
               \x20   s = socket.socket()\n\
               \x20   s.settimeout(2)\n\
               \x20   try:\n\
               \x20       s.connect((host, 53))\n\
               \x20       print('connected', host)\n\
               \x20   except OSError:\n\
               \x20       print('blocked')\n";
    let o = r.run(src, 1);
    assert!(o.ok(), "{o:?}");
    assert!(
        !o.stdout.contains("connected"),
        "network reachable in {}",
        r.name()
    );
}

fn spin_loop_times_out(r: &dyn Runner) {
    let o = r.run("while True:\n    pass\n", 1);
    assert!(o.timed_out, "{o:?}");
    let oracle = Oracle::Baseline {
        sha256: String::new(),
    };
    assert_eq!(
        judge(1_000_000_000, &oracle, &o, 0.05),
        Decision::RejectTimeout
    );
}

fn timing_comes_from_inside(r: &dyn Runner) {
    let o = r.run("x = sum(range(1000))\nprint(x)\n", 3);
    assert!(o.ok(), "{o:?}");
    assert_eq!(o.times_ns.len(), 3);
    // Sandbox startup is tens of milliseconds or more; the timed body is not.
    assert!(
        o.median_ns < 20_000_000,
        "median {} ns includes startup",
        o.median_ns
    );
    assert!(o.peak_rss_kb > 0);
    assert_eq!(o.stdout, "499500\n");
}

macro_rules! suite {
    ($modname:ident, $make:expr) => {
        mod $modname {
            use super::*;
            #[test]
            fn sentinel_env_absent() {
                if let Some(r) = $make {
                    env_is_clean(&r);
                }
            }
            #[test]
            fn host_file_unreadable() {
                if let Some(r) = $make {
                    host_file_is_unreadable(&r);
                }
            }
            #[test]
            fn network_connect_fails() {
                if let Some(r) = $make {
                    network_is_unreachable(&r);
                }
            }
            #[test]
            fn spin_loop_hits_timeout() {
                if let Some(r) = $make {
                    spin_loop_times_out(&r);
                }
            }
            #[test]
            fn timing_is_in_sandbox() {
                if let Some(r) = $make {
                    timing_comes_from_inside(&r);
                }
            }
        }
    };
}

suite!(unshare_isolation, unshare());
suite!(docker_isolation, docker());

#[test]
fn disabled_userns_fails_closed_with_named_settings() {
    let dir = std::env::temp_dir().join(format!("aco-fake-unshare-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let fake = dir.join("unshare");
    std::fs::write(
        &fake,
        "#!/bin/sh\necho 'unshare: unshare failed: Operation not permitted' >&2\nexit 1\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

    let r = UnshareRunner {
        unshare_bin: fake,
        python: "/usr/bin/python3".into(),
        limits: limits(),
    };
    let err = r.probe().unwrap_err();
    assert!(err.contains("Operation not permitted"), "{err}");
    assert!(err.contains("kernel.unprivileged_userns_clone"), "{err}");
    assert!(
        err.contains("kernel.apparmor_restrict_unprivileged_userns"),
        "{err}"
    );
    assert!(err.contains("--sandbox docker"), "{err}");
    // And a run through the broken sandbox is an error, never an unsandboxed pass.
    let o = r.run("print(1)\n", 1);
    assert!(!o.ok());
    assert!(o.error.unwrap().contains("sandbox produced no result"));
    assert!(userns_unavailable("x").contains("aco never changes them"));
    let _ = std::fs::remove_dir_all(dir);
}
