//! Live Ratatui dashboard and the `--plain` line renderer.
//!
//! Both render the same [`Event`] stream from the search. Neither re-parses
//! the file.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use crossterm::event::{self, Event as TermEvent, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Paragraph, Row, Sparkline, Table};
use ratatui::Frame;

use crate::score::Decision;
use crate::search::Event;

pub fn ms(ns: u64) -> String {
    let v = ns as f64 / 1e6;
    if v < 10.0 {
        format!("{v:.3} ms")
    } else {
        format!("{v:.1} ms")
    }
}

pub fn mb(kb: u64) -> String {
    format!("{:.1} MB", kb as f64 / 1024.0)
}

/// One `--plain` line per event.
pub fn plain_line(e: &Event) -> String {
    match e {
        Event::Start { file, sandbox, proposer } => {
            format!("== {file}  sandbox={sandbox}  proposer={proposer}")
        }
        Event::Baseline(m) => format!(
            "baseline          median {}  rss {}  reps {}",
            ms(m.median_ns),
            mb(m.peak_rss_kb),
            m.times_ns.len()
        ),
        Event::Candidate(c) => {
            let time = c.median_ns.map(ms).unwrap_or_else(|| "-".into());
            let rss = c.peak_rss_kb.map(mb).unwrap_or_else(|| "-".into());
            let speed = c.speedup.map(|s| format!("{s:.2}x")).unwrap_or_else(|| "-".into());
            let reward = c.reward.map(|r| format!("{r:.3}")).unwrap_or_else(|| "-".into());
            let mut line = format!(
                "gen {} #{:<2} {:<28} median {:>11}  rss {:>8}  tokens {:>4}  speedup {:>8}  reward {:>8}  {}",
                c.gen,
                c.index,
                c.origin,
                time,
                rss,
                c.tokens,
                speed,
                reward,
                c.decision.label()
            );
            if c.decision != Decision::Keep {
                if let Some(d) = &c.detail {
                    line.push_str(&format!(" ({d})"));
                }
            }
            line
        }
        Event::Generation(g) => format!(
            "gen {} done: {} evaluated, {} kept, elite {} at {}{}",
            g.gen,
            g.evaluated,
            g.kept,
            g.best_origin,
            ms(g.best_median_ns),
            if g.new_elite { " (new elite)" } else { "" }
        ),
        Event::Note(n) => format!("note: {n}"),
        Event::Stop(s) => format!("stop: {s}"),
        Event::ConfirmStart { origin, rounds } => format!(
            "confirm {origin}: {rounds} interleaved rounds (baseline, candidate, ...) in fresh sandboxes"
        ),
        Event::Confirm(c) => format!(
            "confirm           baseline median {}  candidate median {}  speedup {:.2}x  {}",
            ms(c.baseline_median_ns),
            ms(c.candidate_median_ns),
            c.speedup,
            if c.passed {
                "CONFIRMED".to_string()
            } else {
                format!("NOT CONFIRMED ({})", c.detail.clone().unwrap_or_default())
            }
        ),
        Event::Best(b) => format!(
            "best: {}  median {}  speedup {:.2}x  ({})",
            b.origin,
            ms(b.median_ns),
            b.speedup,
            b.reason
        ),
    }
}

struct TableRow {
    gen: String,
    origin: String,
    time: String,
    rss: String,
    tokens: String,
    decision: String,
    keep: bool,
}

/// Dashboard state, built only from events.
#[derive(Default)]
pub struct Dashboard {
    title: String,
    status: String,
    rows: Vec<TableRow>,
    best_us: Vec<u64>,
    rss_kb: Vec<u64>,
    tokens: Vec<u64>,
    pub done: bool,
}

impl Dashboard {
    pub fn apply(&mut self, e: &Event) {
        match e {
            Event::Start { file, sandbox, proposer } => {
                self.title = format!(" aco  {file}  sandbox={sandbox}  proposer={proposer} ");
                self.best_us.clear();
                self.rss_kb.clear();
                self.tokens.clear();
                self.rows.push(TableRow {
                    gen: "".into(),
                    origin: file.clone(),
                    time: "".into(),
                    rss: "".into(),
                    tokens: "".into(),
                    decision: "".into(),
                    keep: false,
                });
                self.status = "measuring baseline".into();
            }
            Event::Baseline(m) => {
                self.best_us.push(m.median_ns / 1000);
                self.rss_kb.push(m.peak_rss_kb);
                self.tokens.push(0);
                self.rows.push(TableRow {
                    gen: "0".into(),
                    origin: "baseline".into(),
                    time: ms(m.median_ns),
                    rss: mb(m.peak_rss_kb),
                    tokens: "0".into(),
                    decision: "reference".into(),
                    keep: false,
                });
                self.status = "generation 1".into();
            }
            Event::Candidate(c) => self.rows.push(TableRow {
                gen: c.gen.to_string(),
                origin: c.origin.clone(),
                time: c.median_ns.map(ms).unwrap_or_else(|| "-".into()),
                rss: c.peak_rss_kb.map(mb).unwrap_or_else(|| "-".into()),
                tokens: c.tokens.to_string(),
                decision: c.decision.label().into(),
                keep: c.decision == Decision::Keep,
            }),
            Event::Generation(g) => {
                self.best_us.push(g.best_median_ns / 1000);
                self.rss_kb.push(g.best_peak_rss_kb);
                self.tokens.push(g.tokens as u64);
                self.status = format!("generation {} done", g.gen);
            }
            Event::ConfirmStart { origin, rounds } => {
                self.status = format!("confirming {origin} ({rounds} interleaved rounds)");
            }
            Event::Confirm(c) => self.rows.push(TableRow {
                gen: "-".into(),
                origin: format!("confirmed {}", c.origin),
                time: ms(c.candidate_median_ns),
                rss: "".into(),
                tokens: "".into(),
                decision: if c.passed {
                    format!("CONFIRMED {:.2}x vs {}", c.speedup, ms(c.baseline_median_ns))
                } else {
                    "NOT CONFIRMED".into()
                },
                keep: c.passed,
            }),
            Event::Stop(s) => self.status = s.clone(),
            Event::Note(n) => self.status = n.clone(),
            Event::Best(b) => self.status = format!("best: {} ({:.2}x)  {}", b.origin, b.speedup, b.reason),
        }
    }

    pub fn draw(&self, f: &mut Frame) {
        let [head, sparks, table, foot] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(7),
            Constraint::Min(5),
            Constraint::Length(1),
        ])
        .areas(f.area());
        f.render_widget(
            Paragraph::new(Line::from(self.title.clone())).style(Style::new().add_modifier(Modifier::BOLD)),
            head,
        );
        let [a, b, c] = Layout::horizontal([Constraint::Ratio(1, 3); 3]).areas(sparks);
        let spark = |title: String, data: &[u64], color: Color| {
            Sparkline::default()
                .block(Block::bordered().title(title))
                .data(data.to_vec())
                .style(Style::new().fg(color))
        };
        let last = |v: &[u64]| v.last().copied().unwrap_or(0);
        f.render_widget(
            spark(format!("best median per gen ({})", ms(last(&self.best_us) * 1000)), &self.best_us, Color::Cyan),
            a,
        );
        f.render_widget(
            spark(format!("peak RSS ({})", mb(last(&self.rss_kb))), &self.rss_kb, Color::Magenta),
            b,
        );
        f.render_widget(
            spark(format!("tokens per gen ({})", last(&self.tokens)), &self.tokens, Color::Yellow),
            c,
        );
        let visible = (table.height as usize).saturating_sub(3);
        let start = self.rows.len().saturating_sub(visible);
        let rows = self.rows[start..].iter().map(|r| {
            let style = if r.keep { Style::new().fg(Color::Green) } else { Style::new() };
            Row::new(vec![
                r.gen.clone(),
                r.origin.clone(),
                r.time.clone(),
                r.rss.clone(),
                r.tokens.clone(),
                r.decision.clone(),
            ])
            .style(style)
        });
        let t = Table::new(
            rows,
            [
                Constraint::Length(4),
                Constraint::Min(28),
                Constraint::Length(12),
                Constraint::Length(9),
                Constraint::Length(7),
                Constraint::Min(24),
            ],
        )
        .header(Row::new(vec!["gen", "candidate", "median", "RSS", "tokens", "decision"]).style(Style::new().add_modifier(Modifier::BOLD)))
        .block(Block::bordered().title("candidates"));
        f.render_widget(t, table);
        let hint = if self.done { "done, press q to exit" } else { "q to cancel" };
        f.render_widget(Paragraph::new(format!(" {}   [{hint}]", self.status)), foot);
    }
}

/// Run `work` on a worker thread and render its events live until it finishes
/// and the user presses `q`. `q` or Ctrl-C during the run sets `cancel`.
pub fn run_dashboard<T: Send>(
    cancel: &AtomicBool,
    work: impl FnOnce(&mut dyn FnMut(Event)) -> T + Send,
) -> io::Result<T> {
    let (tx, rx) = mpsc::channel::<Event>();
    std::thread::scope(|s| {
        let handle = s.spawn(move || {
            let mut emit = |e: Event| {
                let _ = tx.send(e);
            };
            work(&mut emit)
        });
        let mut terminal = ratatui::init();
        let mut dash = Dashboard::default();
        let result = (|| -> io::Result<()> {
            loop {
                while let Ok(e) = rx.try_recv() {
                    dash.apply(&e);
                }
                dash.done = handle.is_finished();
                terminal.draw(|f| dash.draw(f))?;
                if event::poll(Duration::from_millis(60))? {
                    if let TermEvent::Key(k) = event::read()? {
                        let quit = k.kind == KeyEventKind::Press
                            && (k.code == KeyCode::Char('q')
                                || (k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL)));
                        if quit {
                            if dash.done {
                                return Ok(());
                            }
                            cancel.store(true, Ordering::SeqCst);
                            dash.status = "cancelling after the current candidate...".into();
                        }
                    }
                }
                if dash.done && cancel.load(Ordering::SeqCst) {
                    return Ok(());
                }
            }
        })();
        ratatui::restore();
        let out = handle.join().expect("search thread panicked");
        result.map(|_| out)
    })
}
