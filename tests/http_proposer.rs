//! The model proposer against a local mock OpenAI-compatible server.

mod common;

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::thread;

use aco::propose::http::HttpProposer;
use aco::propose::Proposer;
use aco::search::{optimize, Config};
use common::{no_cancel, FakeRunner};

const BUBBLE: &str = include_str!("../fixtures/bubble.py");

const FASTER: &str = "seed = 12345\ndata = []\nfor _ in range(3500):\n    seed = (seed * 1103515245 + 12345) % 2147483648\n    data.append(seed % 100000)\nout = sorted(data)\nprint(out[0], out[len(out) // 2], out[-1], sum(out[:100]))\n";
const VIOLATOR: &str = "import os\nos.system('echo pwned')\n";

/// Serves canned chat replies, alternating faster / violator, and records request bodies.
fn mock_server() -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let seen = bodies.clone();
    thread::spawn(move || {
        for (n, stream) in listener.incoming().enumerate() {
            let Ok(mut s) = stream else { break };
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            let body = loop {
                let k = s.read(&mut chunk).unwrap_or(0);
                if k == 0 {
                    break String::new();
                }
                buf.extend_from_slice(&chunk[..k]);
                let text = String::from_utf8_lossy(&buf).to_string();
                if let Some(h) = text.find("\r\n\r\n") {
                    let len = text[..h]
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if buf.len() >= h + 4 + len {
                        break String::from_utf8_lossy(&buf[h + 4..h + 4 + len]).to_string();
                    }
                }
            };
            seen.lock().unwrap().push(body);
            let reply = if n % 2 == 0 {
                serde_json::json!({
                    "choices": [{"message": {"role": "assistant", "content": format!("```python\n{FASTER}```")}}],
                    "usage": {"prompt_tokens": 300, "completion_tokens": 96, "total_tokens": 396}
                })
            } else {
                // No usage field: tokens are approximated from bytes.
                serde_json::json!({"choices": [{"message": {"role": "assistant", "content": format!("```python\n{VIOLATOR}```")}}]})
            }
            .to_string();
            let _ = write!(
                s,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                reply.len(),
                reply
            );
        }
    });
    (format!("http://{addr}"), bodies)
}

#[test]
fn violator_is_dropped_and_faster_variant_is_returned() {
    let (endpoint, bodies) = mock_server();
    let p = HttpProposer::new(&endpoint, "qwen2.5-coder:1.5b", 512, 2);
    let cands = p.propose(BUBBLE);
    assert_eq!(cands.len(), 1);
    assert_eq!(cands[0].source, FASTER);
    assert_eq!(cands[0].tokens, 96);
    assert_eq!(cands[0].origin, "model:qwen2.5-coder:1.5b#0");
    let notes = p.take_notes();
    assert!(
        notes
            .iter()
            .any(|n| n.contains("dropped by policy") && n.contains("`os`")),
        "{notes:?}"
    );
    let req: serde_json::Value = serde_json::from_str(&bodies.lock().unwrap()[0]).unwrap();
    assert_eq!(req["model"], "qwen2.5-coder:1.5b");
    assert_eq!(req["max_tokens"], 512);
    assert!(req["messages"][1]["content"]
        .as_str()
        .unwrap()
        .contains("def bubble"));
}

#[test]
fn violator_never_reaches_the_runner() {
    let (endpoint, _) = mock_server();
    let p = HttpProposer::new(&endpoint, "qwen2.5-coder:1.5b", 512, 2);
    let runner = FakeRunner::new(400_000_000, vec![("out = sorted(data)", 2_000_000, "same")]);
    let cfg = Config::default();
    let r = optimize(
        "bubble.py",
        BUBBLE,
        &p,
        &runner,
        &cfg,
        None,
        &no_cancel(),
        &mut |_| {},
    )
    .unwrap();
    assert!(runner
        .seen
        .lock()
        .unwrap()
        .iter()
        .all(|s| !s.contains("import os")));
    assert!(r.best.confirmed);
    assert!(r.best.origin.starts_with("model:"));
    let kept = r
        .candidates
        .iter()
        .find(|c| c.origin.starts_with("model:"))
        .unwrap();
    assert_eq!(kept.tokens, 96);
    // 96 / 512 tokens costs 0.05 * 0.1875 of reward.
    let expected = kept.speedup.unwrap() - 0.15 * 1.0 - 0.05 * (96.0 / 512.0);
    assert!((kept.reward.unwrap() - expected).abs() < 1e-9);
}

#[test]
fn unreachable_server_is_a_note_not_a_crash() {
    let p = HttpProposer::new("http://127.0.0.1:9", "m", 512, 3);
    assert!(p.propose(BUBBLE).is_empty());
    assert!(p.take_notes()[0].contains("failed"));
}
