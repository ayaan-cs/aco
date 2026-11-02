//! The live Ratatui dashboard.
//!
//! It renders the same [`Event`] stream as `--plain`; nothing on screen is
//! computed from anything else. The terminal is restored on normal exit, on
//! `q`/Ctrl-C, on SIGINT/SIGTERM (via the cancel flag) and on panic.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use crossterm::event::{self, Event as TermEvent, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Gauge, Padding, Paragraph, Row, Sparkline, Table, Wrap};
use ratatui::Frame;

use crate::score::Decision;
use crate::search::{Config, Event, Summary};
use crate::ui::{decision_text, diff, dur, mb, pct, speedup, DiffKind};

/// Smallest terminal the layout is designed for.
pub const MIN_W: u16 = 80;
pub const MIN_H: u16 = 24;

#[derive(Clone, Debug, PartialEq)]
enum Phase {
    Idle,
    Baseline,
    Search {
        gen: usize,
        done: usize,
        batch: usize,
    },
    Confirm {
        done: usize,
        rounds: usize,
    },
    Finished,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RowKind {
    Baseline,
    Candidate(Decision),
    Confirmed(bool),
}

struct TableRow {
    gen: String,
    origin: String,
    time: String,
    rss: String,
    tokens: String,
    speedup: String,
    decision: String,
    kind: RowKind,
}

/// Dashboard state, built only from events.
pub struct Dashboard {
    total_files: usize,
    file_no: usize,
    file: String,
    sandbox: String,
    proposer: String,
    config: Config,
    phase: Phase,
    status: String,
    rows: Vec<TableRow>,
    best_us: Vec<u64>,
    rss_kb: Vec<u64>,
    tokens: Vec<u64>,
    summaries: Vec<Summary>,
    /// Rows scrolled up from the bottom of the candidate table.
    pub scroll: usize,
    pub cancelling: bool,
    pub done: bool,
}

impl Default for Dashboard {
    fn default() -> Self {
        Dashboard::new(1)
    }
}

impl Dashboard {
    pub fn new(total_files: usize) -> Self {
        Dashboard {
            total_files: total_files.max(1),
            file_no: 0,
            file: String::new(),
            sandbox: String::new(),
            proposer: String::new(),
            config: Config::default(),
            phase: Phase::Idle,
            status: "starting".into(),
            rows: Vec::new(),
            best_us: Vec::new(),
            rss_kb: Vec::new(),
            tokens: Vec::new(),
            summaries: Vec::new(),
            scroll: 0,
            cancelling: false,
            done: false,
        }
    }

    pub fn summaries(&self) -> &[Summary] {
        &self.summaries
    }

    fn current_summary(&self) -> Option<&Summary> {
        self.summaries.last().filter(|s| s.file == self.file)
    }

    pub fn apply(&mut self, e: &Event) {
        match e {
            Event::Start {
                file,
                sandbox,
                proposer,
                config,
            } => {
                self.file_no += 1;
                self.file = file.clone();
                self.sandbox = sandbox.clone();
                self.proposer = proposer.clone();
                self.config = config.clone();
                self.rows.clear();
                self.best_us.clear();
                self.rss_kb.clear();
                self.tokens.clear();
                self.scroll = 0;
                self.phase = Phase::Baseline;
                self.status = "measuring the baseline".into();
            }
            Event::Baseline(m) => {
                self.best_us.push(m.median_ns / 1000);
                self.rss_kb.push(m.peak_rss_kb);
                self.tokens.push(0);
                self.rows.push(TableRow {
                    gen: "0".into(),
                    origin: "baseline".into(),
                    time: dur(m.median_ns),
                    rss: mb(m.peak_rss_kb),
                    tokens: "0".into(),
                    speedup: "1.00x".into(),
                    decision: "reference".into(),
                    kind: RowKind::Baseline,
                });
                self.status = "baseline measured".into();
            }
            Event::GenStart { gen, batch } => {
                self.phase = Phase::Search {
                    gen: *gen,
                    done: 0,
                    batch: *batch,
                };
                self.status = if *batch == 0 {
                    format!("generation {gen}: no new candidates")
                } else {
                    format!("generation {gen}: measuring {batch} candidates")
                };
            }
            Event::Candidate(c) => {
                if let Phase::Search { done, .. } = &mut self.phase {
                    *done += 1;
                }
                self.rows.push(TableRow {
                    gen: c.gen.to_string(),
                    origin: c.origin.clone(),
                    time: c.median_ns.map(dur).unwrap_or_else(|| "-".into()),
                    rss: c.peak_rss_kb.map(mb).unwrap_or_else(|| "-".into()),
                    tokens: c.tokens.to_string(),
                    speedup: c.speedup.map(speedup).unwrap_or_else(|| "-".into()),
                    decision: decision_text(c.decision, self.config.margin),
                    kind: RowKind::Candidate(c.decision),
                });
            }
            Event::Generation(g) => {
                self.best_us.push(g.best_median_ns / 1000);
                self.rss_kb.push(g.best_peak_rss_kb);
                self.tokens.push(g.tokens as u64);
                self.status = if g.new_elite {
                    format!("generation {}: new elite {}", g.gen, g.best_origin)
                } else {
                    format!("generation {}: elite unchanged", g.gen)
                };
            }
            Event::ConfirmStart { origin, rounds } => {
                self.phase = Phase::Confirm {
                    done: 0,
                    rounds: *rounds,
                };
                self.status = format!("confirming {origin} against the baseline");
            }
            Event::ConfirmRound { done, rounds } => {
                self.phase = Phase::Confirm {
                    done: *done,
                    rounds: *rounds,
                };
            }
            Event::Confirm(c) => self.rows.push(TableRow {
                gen: "✓".into(),
                origin: format!("confirm {}", c.origin),
                time: dur(c.candidate_median_ns),
                rss: String::new(),
                tokens: String::new(),
                speedup: speedup(c.speedup),
                decision: if c.passed {
                    format!("confirmed vs {}", dur(c.baseline_median_ns))
                } else {
                    format!("not confirmed: {}", c.detail.clone().unwrap_or_default())
                },
                kind: RowKind::Confirmed(c.passed),
            }),
            Event::Stop(s) => self.status = s.clone(),
            Event::Note(n) => self.status = n.clone(),
            Event::Best(b) => {
                self.status = if b.confirmed {
                    format!("best: {} at {}", b.origin, speedup(b.speedup))
                } else {
                    format!("original kept: {}", b.reason)
                }
            }
            Event::Finished(s) => {
                self.phase = Phase::Finished;
                self.summaries.push((**s).clone());
            }
        }
    }

    fn progress(&self) -> (f64, String) {
        let files = if self.total_files > 1 {
            format!("file {}/{} · ", self.file_no.max(1), self.total_files)
        } else {
            String::new()
        };
        let gens = self.config.gens.max(1);
        let (ratio, what) = match &self.phase {
            Phase::Idle => (0.0, "starting".to_string()),
            Phase::Baseline => (0.0, "measuring baseline".to_string()),
            Phase::Search { gen, done, batch } => {
                let within = if *batch == 0 {
                    1.0
                } else {
                    *done as f64 / *batch as f64
                };
                (
                    ((*gen as f64 - 1.0) + within) / gens as f64,
                    format!("generation {gen}/{gens} · candidate {done}/{batch}"),
                )
            }
            Phase::Confirm { done, rounds } => (
                *done as f64 / (*rounds).max(1) as f64,
                format!("confirming · round {done}/{rounds}"),
            ),
            Phase::Finished => (1.0, "finished".to_string()),
        };
        let label = if self.cancelling && !self.done {
            format!("{files}cancelling…")
        } else if self.done {
            format!("{files}done")
        } else {
            format!("{files}{what}")
        };
        (ratio.clamp(0.0, 1.0), label)
    }

    pub fn draw(&self, f: &mut Frame) {
        let area = f.area();
        if area.width < MIN_W || area.height < MIN_H {
            let msg = Paragraph::new(vec![
                Line::from(Span::styled(
                    "terminal too small",
                    Style::new().add_modifier(Modifier::BOLD),
                )),
                Line::from(format!(
                    "{}x{} now, aco needs at least {MIN_W}x{MIN_H}",
                    area.width, area.height
                )),
                Line::from("resize, or press q to quit"),
            ])
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true });
            let y = area.height.saturating_sub(3) / 2;
            f.render_widget(
                msg,
                Rect::new(area.x, area.y + y, area.width, 3.min(area.height)),
            );
            return;
        }
        let summary = self.current_summary();
        let bottom_h = if summary.is_some() {
            (area.height.saturating_sub(4 + 3 + 6 + 1 + 6)).clamp(8, 14)
        } else {
            0
        };
        let [head, gauge, sparks, table, bottom, foot] = Layout::vertical([
            Constraint::Length(4),
            Constraint::Length(3),
            Constraint::Length(6),
            Constraint::Min(6),
            Constraint::Length(bottom_h),
            Constraint::Length(1),
        ])
        .areas(area);
        self.draw_header(f, head);
        let (ratio, label) = self.progress();
        let color = if self.done { Color::Green } else { Color::Cyan };
        f.render_widget(
            Gauge::default()
                .block(Block::bordered().title(" progress "))
                .gauge_style(Style::new().fg(color).bg(Color::Black))
                .ratio(ratio)
                .label(Span::styled(
                    label,
                    Style::new().fg(Color::White).add_modifier(Modifier::BOLD),
                )),
            gauge,
        );
        self.draw_sparks(f, sparks);
        self.draw_table(f, table);
        if let Some(s) = summary {
            let [left, right] =
                Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)])
                    .areas(bottom);
            draw_summary(f, left, s);
            draw_diff(f, right, s);
        }
        let hint = if self.done {
            "q quit · ↑↓ scroll"
        } else {
            "q/Ctrl-C cancel · ↑↓ scroll"
        };
        let [st, keys] = Layout::horizontal([
            Constraint::Min(10),
            Constraint::Length(hint.chars().count() as u16 + 2),
        ])
        .areas(foot);
        f.render_widget(
            Paragraph::new(format!(" {}", self.status)).style(Style::new().fg(Color::Gray)),
            st,
        );
        f.render_widget(
            Paragraph::new(hint)
                .alignment(Alignment::Right)
                .style(Style::new().fg(Color::DarkGray)),
            keys,
        );
    }

    fn draw_header(&self, f: &mut Frame, area: Rect) {
        let key = |s: &'static str| Span::styled(s, Style::new().fg(Color::DarkGray));
        let val = |s: String| Span::styled(s, Style::new().add_modifier(Modifier::BOLD));
        let file = if self.total_files > 1 {
            format!(
                "{} ({}/{})",
                self.file,
                self.file_no.max(1),
                self.total_files
            )
        } else {
            self.file.clone()
        };
        let c = &self.config;
        let lines = vec![
            Line::from(vec![
                key("file "),
                val(file),
                key("   sandbox "),
                Span::styled(self.sandbox.clone(), Style::new().fg(Color::Cyan)),
                key("   proposer "),
                Span::styled(self.proposer.clone(), Style::new().fg(Color::Cyan)),
            ]),
            Line::from(vec![
                key("search "),
                Span::raw(format!(
                    "{} gens · pop {} · patience {} · {} reps · margin {} · {} jobs",
                    c.gens,
                    c.pop,
                    c.patience,
                    c.reps,
                    pct(c.margin),
                    c.jobs
                )),
            ]),
        ];
        let title = Line::from(vec![
            Span::styled(
                " aco ",
                Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                concat!(env!("CARGO_PKG_VERSION"), " "),
                Style::new().fg(Color::DarkGray),
            ),
        ]);
        f.render_widget(
            Paragraph::new(lines).block(
                Block::bordered()
                    .title(title)
                    .padding(Padding::horizontal(1)),
            ),
            area,
        );
    }

    fn draw_sparks(&self, f: &mut Frame, area: Rect) {
        let [a, b, c] = Layout::horizontal([Constraint::Ratio(1, 3); 3]).areas(area);
        let last = |v: &[u64]| v.last().copied();
        let spark = |title: String, data: &[u64], color: Color| {
            Sparkline::default()
                .block(Block::bordered().title(title))
                .data(data.to_vec())
                .style(Style::new().fg(color))
        };
        let t = last(&self.best_us)
            .map(|v| dur(v * 1000))
            .unwrap_or_else(|| "-".into());
        f.render_widget(
            spark(
                format!(" best median per gen · {t} "),
                &self.best_us,
                Color::Cyan,
            ),
            a,
        );
        let r = last(&self.rss_kb).map(mb).unwrap_or_else(|| "-".into());
        f.render_widget(
            spark(format!(" peak RSS · {r} "), &self.rss_kb, Color::Magenta),
            b,
        );
        let k = last(&self.tokens)
            .map(|v| v.to_string())
            .unwrap_or_else(|| "-".into());
        f.render_widget(
            spark(
                format!(" tokens per gen · {k} "),
                &self.tokens,
                Color::Yellow,
            ),
            c,
        );
    }

    fn draw_table(&self, f: &mut Frame, area: Rect) {
        let visible = (area.height as usize).saturating_sub(3).max(1);
        let max_scroll = self.rows.len().saturating_sub(visible);
        let scroll = self.scroll.min(max_scroll);
        let end = self.rows.len() - scroll;
        let start = end.saturating_sub(visible);
        let rows = self.rows[start..end].iter().map(|r| {
            let (row_style, dec_style) = match r.kind {
                RowKind::Baseline => (
                    Style::new().add_modifier(Modifier::BOLD),
                    Style::new().fg(Color::DarkGray),
                ),
                RowKind::Candidate(Decision::Keep) => (
                    Style::new().fg(Color::Green),
                    Style::new().fg(Color::Green).add_modifier(Modifier::BOLD),
                ),
                RowKind::Candidate(Decision::RejectMargin) => {
                    (Style::new(), Style::new().fg(Color::Yellow))
                }
                RowKind::Candidate(_) => (Style::new(), Style::new().fg(Color::Red)),
                RowKind::Confirmed(true) => (
                    Style::new().fg(Color::Green).add_modifier(Modifier::BOLD),
                    Style::new().fg(Color::Green).add_modifier(Modifier::BOLD),
                ),
                RowKind::Confirmed(false) => (
                    Style::new().fg(Color::Red),
                    Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
                ),
            };
            Row::new(vec![
                Cell::from(r.gen.clone()),
                Cell::from(r.origin.clone()),
                Cell::from(Line::from(r.time.clone()).alignment(Alignment::Right)),
                Cell::from(Line::from(r.rss.clone()).alignment(Alignment::Right)),
                Cell::from(Line::from(r.tokens.clone()).alignment(Alignment::Right)),
                Cell::from(Line::from(r.speedup.clone()).alignment(Alignment::Right)),
                Cell::from(Span::styled(r.decision.clone(), dec_style)),
            ])
            .style(row_style)
        });
        let header = Row::new(vec![
            Cell::from("gen"),
            Cell::from("candidate"),
            Cell::from(Line::from("median").alignment(Alignment::Right)),
            Cell::from(Line::from("RSS").alignment(Alignment::Right)),
            Cell::from(Line::from("tokens").alignment(Alignment::Right)),
            Cell::from(Line::from("speedup").alignment(Alignment::Right)),
            Cell::from("decision"),
        ])
        .style(
            Style::new()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        );
        let title = if scroll > 0 {
            format!(
                " candidates · {} rows · scrolled {scroll} up ",
                self.rows.len()
            )
        } else {
            format!(" candidates · {} rows ", self.rows.len())
        };
        let t = Table::new(
            rows,
            [
                Constraint::Length(3),
                Constraint::Min(24),
                Constraint::Length(10),
                Constraint::Length(8),
                Constraint::Length(6),
                Constraint::Length(8),
                Constraint::Min(22),
            ],
        )
        .column_spacing(2)
        .header(header)
        .block(
            Block::bordered()
                .title(title)
                .padding(Padding::horizontal(1)),
        );
        f.render_widget(t, area);
    }

    pub fn scroll_by(&mut self, delta: isize) {
        let s = self.scroll as isize + delta;
        self.scroll = s.clamp(0, self.rows.len() as isize) as usize;
    }
}

fn draw_summary(f: &mut Frame, area: Rect, s: &Summary) {
    let key = |t: &'static str| Span::styled(format!("{t:<10} "), Style::new().fg(Color::DarkGray));
    let mut lines = Vec::new();
    if s.confirmed {
        lines.push(Line::from(Span::styled(
            format!("✓ {} faster, confirmed", speedup(s.speedup)),
            Style::new().fg(Color::Green).add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(vec![
            key("baseline"),
            Span::raw(dur(s.confirm_baseline_ns.unwrap_or(s.baseline_ns))),
            Span::styled(
                format!("  {} runs", s.rounds.unwrap_or(0)),
                Style::new().fg(Color::DarkGray),
            ),
        ]));
        lines.push(Line::from(vec![
            key("optimized"),
            Span::styled(
                dur(s.best_ns),
                Style::new().fg(Color::Green).add_modifier(Modifier::BOLD),
            ),
        ]));
        lines.push(Line::from(vec![
            key("winner"),
            Span::raw(s.best_origin.clone()),
        ]));
    } else {
        lines.push(Line::from(Span::styled(
            "– no faster version, original kept",
            Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(vec![
            key("baseline"),
            Span::raw(dur(s.baseline_ns)),
        ]));
        lines.push(Line::from(vec![key("reason"), Span::raw(s.reason.clone())]));
    }
    lines.push(Line::from(vec![
        key("searched"),
        Span::raw(format!("{} candidates, {} kept", s.evaluated, s.kept)),
    ]));
    let room = (area.width as usize).saturating_sub(4 + 11).max(12);
    for p in &s.written {
        let k = if p.extension().is_some_and(|e| e == "json") {
            key("report")
        } else {
            key("best.py")
        };
        lines.push(Line::from(vec![
            k,
            Span::styled(
                clip_left(&p.display().to_string(), room),
                Style::new().fg(Color::Cyan),
            ),
        ]));
    }
    lines.push(Line::from(Span::styled(
        s.stop_reason.clone(),
        Style::new().fg(Color::DarkGray),
    )));
    f.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::bordered()
                .title(" summary ")
                .padding(Padding::horizontal(1)),
        ),
        area,
    );
}

/// Keep the end of a path (the file name matters most) within `max` chars.
fn clip_left(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        s.to_string()
    } else {
        let tail: String = s.chars().skip(n - (max - 1)).collect();
        format!("…{tail}")
    }
}

fn draw_diff(f: &mut Frame, area: Rect, s: &Summary) {
    let d = if s.confirmed {
        diff(&s.original, &s.best_source, 2)
    } else {
        Vec::new()
    };
    let lines: Vec<Line> = if d.is_empty() {
        vec![Line::from(Span::styled(
            "no change: the output file is the original",
            Style::new().fg(Color::DarkGray),
        ))]
    } else {
        d.into_iter()
            .map(|(kind, text)| {
                let style = match kind {
                    DiffKind::Hunk => Style::new().fg(Color::Cyan),
                    DiffKind::Added => Style::new().fg(Color::Green),
                    DiffKind::Removed => Style::new().fg(Color::Red),
                    DiffKind::Context => Style::new().fg(Color::Gray),
                };
                Line::from(Span::styled(text, style))
            })
            .collect()
    };
    f.render_widget(
        Paragraph::new(lines).block(
            Block::bordered()
                .title(" diff · original → best ")
                .padding(Padding::horizontal(1)),
        ),
        area,
    );
}

/// How the dashboard ended.
pub enum Exit<T> {
    /// The work finished (possibly after a cancel request).
    Done(T),
    /// The search thread panicked; the terminal is already restored.
    Panicked(String),
}

/// Run `work` on a worker thread and render its events live until it finishes
/// and the user presses `q`. `q` or Ctrl-C during the run sets `cancel` and
/// calls `on_cancel`; the dashboard then exits as soon as the work returns.
pub fn run_dashboard<T: Send>(
    total_files: usize,
    cancel: &AtomicBool,
    on_cancel: &(dyn Fn() + Sync),
    work: impl FnOnce(&mut dyn FnMut(Event)) -> T + Send,
) -> io::Result<Exit<T>> {
    let (tx, rx) = mpsc::channel::<Event>();
    std::thread::scope(|s| {
        let handle = s.spawn(move || {
            let mut emit = |e: Event| {
                let _ = tx.send(e);
            };
            work(&mut emit)
        });
        // ratatui::init enters the alternate screen and raw mode and installs
        // a panic hook that restores the terminal before the panic message.
        let mut terminal = ratatui::init();
        let mut dash = Dashboard::new(total_files);
        let mut handle = Some(handle);
        let mut output: Option<std::thread::Result<T>> = None;
        let result = (|| -> io::Result<()> {
            loop {
                while let Ok(e) = rx.try_recv() {
                    dash.apply(&e);
                }
                if handle.as_ref().is_some_and(|h| h.is_finished()) {
                    while let Ok(e) = rx.try_recv() {
                        dash.apply(&e);
                    }
                    let out = handle.take().map(|h| h.join()).expect("handle present");
                    let panicked = out.is_err();
                    output = Some(out);
                    if panicked {
                        return Ok(());
                    }
                    dash.done = true;
                }
                if dash.done && cancel.load(Ordering::SeqCst) {
                    return Ok(());
                }
                if !dash.cancelling && cancel.load(Ordering::SeqCst) {
                    dash.cancelling = true;
                    dash.status = "cancelling: stopping sandboxes…".into();
                }
                terminal.draw(|f| dash.draw(f))?;
                if event::poll(Duration::from_millis(50))? {
                    match event::read()? {
                        TermEvent::Key(k) if k.kind != KeyEventKind::Release => {
                            let ctrl_c = k.code == KeyCode::Char('c')
                                && k.modifiers.contains(KeyModifiers::CONTROL);
                            match k.code {
                                _ if ctrl_c
                                    || matches!(k.code, KeyCode::Char('q') | KeyCode::Esc) =>
                                {
                                    if dash.done {
                                        return Ok(());
                                    }
                                    cancel.store(true, Ordering::SeqCst);
                                    on_cancel();
                                }
                                KeyCode::Up | KeyCode::Char('k') => dash.scroll_by(1),
                                KeyCode::Down | KeyCode::Char('j') => dash.scroll_by(-1),
                                KeyCode::PageUp => dash.scroll_by(10),
                                KeyCode::PageDown => dash.scroll_by(-10),
                                KeyCode::End => dash.scroll = 0,
                                _ => {}
                            }
                        }
                        // The next draw uses the new size.
                        TermEvent::Resize(_, _) => terminal.autoresize()?,
                        _ => {}
                    }
                }
            }
        })();
        ratatui::restore();
        // If the loop failed (terminal I/O), stop the work before joining.
        if result.is_err() {
            cancel.store(true, Ordering::SeqCst);
            on_cancel();
        }
        let out = match output {
            Some(o) => o,
            None => handle.take().map(|h| h.join()).expect("handle present"),
        };
        result?;
        Ok(match out {
            Ok(v) => Exit::Done(v),
            Err(p) => Exit::Panicked(
                p.downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| p.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "unknown panic".into()),
            ),
        })
    })
}
