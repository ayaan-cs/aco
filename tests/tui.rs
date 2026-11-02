mod common;

use aco::propose::RuleProposer;
use aco::search::{optimize, Config, Event, Summary};
use aco::tui::Dashboard;
use aco::ui::Plain;
use common::{no_cancel, FakeRunner};
use ratatui::backend::TestBackend;
use ratatui::Terminal;

const BUBBLE: &str = include_str!("../fixtures/bubble.py");

fn events() -> Vec<Event> {
    let runner = FakeRunner::new(400_000_000, vec![("sorted(xs)", 2_000_000, "same")]);
    let mut ev = Vec::new();
    let report = optimize(
        "fixtures/bubble.py",
        BUBBLE,
        &RuleProposer,
        &runner,
        &Config::default(),
        None,
        &no_cancel(),
        &mut |e| ev.push(e),
    )
    .unwrap();
    ev.push(Event::Finished(Box::new(Summary::new(
        &report,
        BUBBLE,
        vec!["aco-out/bubble.best.py".into()],
    ))));
    ev
}

fn screen(d: &Dashboard, w: u16, h: u16) -> String {
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    t.draw(|f| d.draw(f)).unwrap();
    let buf = t.backend().buffer().clone();
    buf.content()
        .chunks(w as usize)
        .map(|row| row.iter().map(|c| c.symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn plain_output_is_aligned_and_uncoloured_when_off() {
    let mut r = Plain::new(false);
    let lines: Vec<String> = events().iter().flat_map(|e| r.render(e)).collect();
    let text = lines.join("\n");
    assert!(!text.contains('\x1b'), "escape codes with colour off");
    assert!(lines[0].starts_with("aco ") && lines[0].ends_with("fixtures/bubble.py"));
    let header = lines.iter().find(|l| l.contains("candidate")).unwrap();
    let kept = lines
        .iter()
        .find(|l| l.contains("rule:bubble-sort:sorted") && l.contains("kept"))
        .unwrap();
    // Columns line up: the median column ends where the header's does.
    let col = |l: &str, word: &str| l.find(word).unwrap() + word.len();
    assert_eq!(col(header, "median"), col(kept, "2.000 ms"));
    assert!(text.contains("✓ confirmed"));
    assert!(text.contains("200.0x faster, confirmed"));
    assert!(text.contains("wrote      aco-out/bubble.best.py"));
    assert!(text.contains("+    xs[:] = sorted(xs)"), "{text}");
}

#[test]
fn plain_output_has_colour_when_on() {
    let mut r = Plain::new(true);
    let text: String = events().iter().flat_map(|e| r.render(e)).collect();
    assert!(text.contains("\x1b[1;32mkept\x1b[0m"));
}

#[test]
fn dashboard_shows_header_gauge_sparklines_table_summary_and_diff() {
    let mut d = Dashboard::new(1);
    for e in events() {
        d.apply(&e);
    }
    d.done = true;
    let text = screen(&d, 120, 40);
    for needle in [
        "aco 0.1.0",
        "file fixtures/bubble.py",
        "sandbox fake",
        "proposer rules",
        "progress",
        "done",
        "best median per gen",
        "peak RSS",
        "tokens per gen",
        "candidates",
        "rule:bubble-sort:sorted",
        "kept",
        "confirm rule:bubble-sort:sorted",
        "summary",
        "200.0x faster, confirmed",
        "best.py    aco-out/bubble.best.py",
        "diff · original → best",
        "+    xs[:] = sorted(xs)",
        "-    for i in range(n):",
        "q quit",
    ] {
        assert!(text.contains(needle), "missing {needle:?} in\n{text}");
    }
}

#[test]
fn dashboard_progress_follows_generations_and_confirmation() {
    let ev = events();
    let mut d = Dashboard::new(1);
    let mut saw_gen = false;
    let mut saw_confirm = false;
    for e in &ev {
        d.apply(e);
        let text = screen(&d, 100, 30);
        saw_gen |= text.contains("generation 1/3 · candidate 1/2");
        saw_confirm |= text.contains("confirming · round 3/10");
    }
    assert!(saw_gen && saw_confirm);
}

#[test]
fn dashboard_survives_any_size_and_says_when_too_small() {
    let mut d = Dashboard::new(1);
    for e in events() {
        d.apply(&e);
    }
    assert!(screen(&d, 60, 20).contains("terminal too small"));
    for (w, h) in [(1, 1), (79, 23), (80, 24), (200, 60), (300, 24)] {
        let _ = screen(&d, w, h);
    }
    assert!(!screen(&d, 80, 24).contains("terminal too small"));
}

#[test]
fn dashboard_scrolls_the_table() {
    let mut d = Dashboard::new(1);
    for e in events() {
        d.apply(&e);
    }
    d.scroll_by(2);
    assert!(screen(&d, 100, 26).contains("scrolled"));
    d.scroll_by(-100);
    assert_eq!(d.scroll, 0);
}
