mod common;

use aco::propose::RuleProposer;
use aco::search::{optimize, Config, Event};
use aco::tui::{plain_line, Dashboard};
use common::{no_cancel, FakeRunner};
use ratatui::backend::TestBackend;
use ratatui::Terminal;

fn events() -> Vec<Event> {
    let runner = FakeRunner::new(400_000_000, vec![("sorted(xs)", 2_000_000, "same")]);
    let mut ev = Vec::new();
    optimize("fixtures/bubble.py", include_str!("../fixtures/bubble.py"), &RuleProposer, &runner, &Config::default(), None, &no_cancel(), &mut |e| ev.push(e)).unwrap();
    ev
}

#[test]
fn plain_prints_one_line_per_event_with_decisions() {
    let ev = events();
    let lines: Vec<String> = ev.iter().map(plain_line).collect();
    assert_eq!(lines.len(), ev.len());
    assert!(lines[0].starts_with("== fixtures/bubble.py  sandbox=fake  proposer=rules"));
    assert!(lines.iter().any(|l| l.contains("rule:bubble-sort:sorted") && l.ends_with("KEEP")));
    assert!(lines.iter().any(|l| l.contains("CONFIRMED")));
    assert!(lines.iter().any(|l| l.starts_with("stop: early stop")));
}

#[test]
fn dashboard_renders_sparklines_and_table() {
    let mut d = Dashboard::default();
    for e in events() {
        d.apply(&e);
    }
    let mut t = Terminal::new(TestBackend::new(120, 30)).unwrap();
    t.draw(|f| d.draw(f)).unwrap();
    let buf = t.backend().buffer().clone();
    let text: String = buf.content().iter().map(|c| c.symbol()).collect();
    for needle in ["best median per gen", "peak RSS", "tokens per gen", "candidates", "rule:bubble-sort:sorted", "KEEP", "confirmed rule:bubble-sort:sorted"] {
        assert!(text.contains(needle), "missing {needle}");
    }
}
