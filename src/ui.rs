//! Shared presentation: colour policy, number formatting, decision labels, a
//! small line diff, and the `--plain` renderer.
//!
//! Everything printed comes from search events; nothing is estimated here.

use std::io::IsTerminal;

use crate::score::Decision;
use crate::search::{Event, Summary};

// ------------------------------------------------------------------ colour

/// `--color` setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum ColorChoice {
    /// Colour when writing to a terminal and NO_COLOR is unset.
    Auto,
    Always,
    Never,
}

/// Decide whether to emit ANSI colour for a stream.
///
/// `auto` follows <https://no-color.org>: a non-empty `NO_COLOR` turns colour
/// off, as do `TERM=dumb` and a stream that is not a terminal.
pub fn color_enabled(choice: ColorChoice, is_tty: bool) -> bool {
    match choice {
        ColorChoice::Always => true,
        ColorChoice::Never => false,
        ColorChoice::Auto => {
            let no_color = std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty());
            let dumb = std::env::var("TERM").is_ok_and(|t| t == "dumb");
            is_tty && !no_color && !dumb
        }
    }
}

pub fn stdout_color(choice: ColorChoice) -> bool {
    color_enabled(choice, std::io::stdout().is_terminal())
}

pub fn stderr_color(choice: ColorChoice) -> bool {
    color_enabled(choice, std::io::stderr().is_terminal())
}

/// Wraps text in ANSI SGR codes when enabled, and is a no-op otherwise.
#[derive(Clone, Copy, Debug)]
pub struct Paint {
    pub on: bool,
}

macro_rules! paint_fn {
    ($name:ident, $code:expr) => {
        pub fn $name(&self, s: impl AsRef<str>) -> String {
            self.sgr($code, s.as_ref())
        }
    };
}

impl Paint {
    pub fn new(on: bool) -> Self {
        Paint { on }
    }
    fn sgr(&self, code: &str, s: &str) -> String {
        if self.on && !s.is_empty() {
            format!("\x1b[{code}m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }
    paint_fn!(bold, "1");
    paint_fn!(dim, "2");
    paint_fn!(red, "31");
    paint_fn!(green, "32");
    paint_fn!(yellow, "33");
    paint_fn!(cyan, "36");
    paint_fn!(magenta, "35");
    paint_fn!(bold_red, "1;31");
    paint_fn!(bold_green, "1;32");
    paint_fn!(bold_yellow, "1;33");
    paint_fn!(bold_cyan, "1;36");
}

// --------------------------------------------------------------- numbers

/// A duration in nanoseconds with a unit that keeps 3-4 significant digits.
/// "3 files · 412 lines · rewriting pkg/helper.py", or "1 file · 23 lines".
pub fn code_text(info: &crate::search::RunInfo) -> String {
    let n = info.files.len().max(1);
    let lines: usize = info.files.iter().map(|f| f.lines).sum();
    let mut s = format!("{n} file{} · {lines} lines", if n == 1 { "" } else { "s" });
    if let Some(root) = &info.project_root {
        s.push_str(&format!(" · root {root}"));
    }
    if let (Some(m), Some(t)) = (&info.mutated, &info.target) {
        if m != t {
            s.push_str(&format!(" · rewriting {m}"));
        }
    }
    if !info.argv.is_empty() {
        s.push_str(&format!(" · args {}", info.argv.join(" ")));
    }
    if let Some(i) = &info.stdin {
        s.push_str(&format!(" · stdin {i}"));
    }
    s
}

/// "numpy 2.1.3, attrs 24.2.0 (from requirements.txt)", or "none".
pub fn packages_text(info: &crate::search::RunInfo) -> String {
    if info.packages.is_empty() {
        return match &info.requirements {
            Some(r) => format!("none installed (from {r})"),
            None => "none (standard library only)".into(),
        };
    }
    let list: Vec<String> = info
        .packages
        .iter()
        .map(|p| format!("{} {}", p.name, p.version))
        .collect();
    match &info.requirements {
        Some(r) => format!("{} (from {r})", list.join(", ")),
        None => list.join(", "),
    }
}

pub fn dur(ns: u64) -> String {
    let v = ns as f64;
    if v >= 1e9 {
        format!("{:.3} s", v / 1e9)
    } else if v >= 1e7 {
        format!("{:.1} ms", v / 1e6)
    } else if v >= 1e6 {
        format!("{:.3} ms", v / 1e6)
    } else if v >= 1e3 {
        format!("{:.1} µs", v / 1e3)
    } else {
        format!("{ns} ns")
    }
}

/// KiB as MB with one decimal.
pub fn mb(kb: u64) -> String {
    format!("{:.1} MB", kb as f64 / 1024.0)
}

pub fn speedup(x: f64) -> String {
    if x >= 10.0 {
        format!("{x:.1}x")
    } else {
        format!("{x:.2}x")
    }
}

pub fn pct(margin: f64) -> String {
    let p = margin * 100.0;
    if (p - p.round()).abs() < 1e-9 {
        format!("{}%", p.round() as i64)
    } else {
        format!("{p:.1}%")
    }
}

/// Human label for a decision; the margin one names the actual margin.
pub fn decision_text(d: Decision, margin: f64) -> String {
    match d {
        Decision::Keep => "kept".into(),
        Decision::RejectPolicy => "rejected: policy".into(),
        Decision::RejectTimeout => "rejected: timeout".into(),
        Decision::RejectError => "rejected: error".into(),
        Decision::RejectOracle => "rejected: output differs".into(),
        Decision::RejectMargin => format!("rejected: not {} faster", pct(margin)),
    }
}

/// Pad to `width` display columns (counting chars), never truncating.
pub fn pad(s: &str, width: usize) -> String {
    let n = s.chars().count();
    if n > width && width > 1 {
        // Clip so the columns stay aligned; the report has the full text.
        let mut out: String = s.chars().take(width - 1).collect();
        out.push('…');
        out
    } else if n == width {
        s.to_string()
    } else {
        format!("{s}{}", " ".repeat(width - n))
    }
}

pub fn rpad(s: &str, width: usize) -> String {
    let n = s.chars().count();
    if n >= width {
        s.to_string()
    } else {
        format!("{}{s}", " ".repeat(width - n))
    }
}

/// Cut to `max` chars with an ellipsis.
pub fn clip(s: &str, max: usize) -> String {
    let s = s.replace('\n', " ");
    if s.chars().count() <= max {
        s
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

// ------------------------------------------------------------------ diff

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffKind {
    Hunk,
    Context,
    Removed,
    Added,
}

/// A unified line diff with `context` lines around each change. Inputs are
/// small source files; above 4,000 lines a side it reports that instead.
pub fn diff(a: &str, b: &str, context: usize) -> Vec<(DiffKind, String)> {
    let a: Vec<&str> = a.lines().collect();
    let b: Vec<&str> = b.lines().collect();
    if a.len() > 4000 || b.len() > 4000 {
        return vec![(DiffKind::Hunk, "(file too large to diff here)".into())];
    }
    let (n, m) = (a.len(), b.len());
    // lcs[i][j] = LCS length of a[i..] and b[j..].
    let mut lcs = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if a[i] == b[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    // Edit script: (kind, a line no, b line no, text).
    let mut ops: Vec<(DiffKind, usize, usize, &str)> = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < n || j < m {
        if i < n && j < m && a[i] == b[j] {
            ops.push((DiffKind::Context, i, j, a[i]));
            i += 1;
            j += 1;
        } else if i < n && (j == m || lcs[i + 1][j] >= lcs[i][j + 1]) {
            ops.push((DiffKind::Removed, i, j, a[i]));
            i += 1;
        } else {
            ops.push((DiffKind::Added, i, j, b[j]));
            j += 1;
        }
    }
    let changed: Vec<usize> = ops
        .iter()
        .enumerate()
        .filter(|(_, o)| o.0 != DiffKind::Context)
        .map(|(k, _)| k)
        .collect();
    if changed.is_empty() {
        return Vec::new();
    }
    // Group changes whose context windows touch.
    let mut groups: Vec<(usize, usize)> = Vec::new();
    for &k in &changed {
        let lo = k.saturating_sub(context);
        let hi = (k + context + 1).min(ops.len());
        match groups.last_mut() {
            Some(g) if lo <= g.1 => g.1 = hi,
            _ => groups.push((lo, hi)),
        }
    }
    let mut out = Vec::new();
    for (lo, hi) in groups {
        let slice = &ops[lo..hi];
        let a_start = slice[0].1 + 1;
        let b_start = slice[0].2 + 1;
        let a_len = slice.iter().filter(|o| o.0 != DiffKind::Added).count();
        let b_len = slice.iter().filter(|o| o.0 != DiffKind::Removed).count();
        out.push((
            DiffKind::Hunk,
            format!("@@ -{a_start},{a_len} +{b_start},{b_len} @@"),
        ));
        for (kind, _, _, text) in slice {
            let sign = match kind {
                DiffKind::Added => '+',
                DiffKind::Removed => '-',
                _ => ' ',
            };
            out.push((*kind, format!("{sign}{text}")));
        }
    }
    out
}

// ----------------------------------------------------------------- plain

const ORIGIN_W: usize = 32;

/// Renders events as aligned lines for `--plain` (and non-terminal stdout).
pub struct Plain {
    pub p: Paint,
    margin: f64,
    files_seen: usize,
    pub finished: Vec<Summary>,
}

impl Plain {
    pub fn new(color: bool) -> Self {
        Plain {
            p: Paint::new(color),
            margin: 0.05,
            files_seen: 0,
            finished: Vec::new(),
        }
    }

    fn table_header(&self) -> String {
        let h = format!(
            "  {} {} {} {} {} {}  {}",
            rpad("gen", 3),
            pad("candidate", ORIGIN_W),
            rpad("median", 10),
            rpad("RSS", 8),
            rpad("tokens", 6),
            rpad("speedup", 8),
            "decision"
        );
        self.p.dim(h)
    }

    /// Lines for one event (possibly none).
    pub fn render(&mut self, e: &Event) -> Vec<String> {
        let p = self.p;
        match e {
            Event::Start {
                file,
                sandbox,
                proposer,
                config,
            } => {
                self.margin = config.margin;
                self.files_seen += 1;
                let mut v = Vec::new();
                if self.files_seen > 1 {
                    v.push(String::new());
                }
                v.push(format!(
                    "{} {} {} {}",
                    p.bold_cyan("aco"),
                    p.dim(env!("CARGO_PKG_VERSION")),
                    p.dim("·"),
                    p.bold(file)
                ));
                v.push(format!(
                    "  {}  {}",
                    p.dim("code    "),
                    code_text(&config.info)
                ));
                v.push(format!(
                    "  {}  {}",
                    p.dim("packages"),
                    packages_text(&config.info)
                ));
                v.push(format!("  {}  {}", p.dim("sandbox "), sandbox));
                v.push(format!("  {}  {}", p.dim("proposer"), proposer));
                v.push(format!(
                    "  {}  {} gens · pop {} · patience {} · {} reps · margin {} · {} jobs",
                    p.dim("search  "),
                    config.gens,
                    config.pop,
                    config.patience,
                    config.reps,
                    pct(config.margin),
                    config.jobs
                ));
                v.push(String::new());
                v.push(self.table_header());
                v
            }
            Event::Baseline(m) => vec![format!(
                "  {} {} {} {} {} {}  {}",
                rpad("0", 3),
                p.bold(pad("baseline", ORIGIN_W)),
                rpad(&dur(m.median_ns), 10),
                rpad(&mb(m.peak_rss_kb), 8),
                rpad("0", 6),
                rpad("1.00x", 8),
                p.dim("reference")
            )],
            Event::GenStart { .. } | Event::ConfirmRound { .. } | Event::Best(_) => Vec::new(),
            Event::Candidate(c) => {
                let decision = decision_text(c.decision, self.margin);
                let decision = match c.decision {
                    Decision::Keep => p.bold_green(decision),
                    Decision::RejectMargin => p.yellow(decision),
                    _ => p.red(decision),
                };
                let detail = match (&c.detail, c.decision) {
                    (Some(d), d2) if d2 != Decision::Keep && d2 != Decision::RejectMargin => {
                        format!("  {}", p.dim(clip(d, 70)))
                    }
                    _ => String::new(),
                };
                vec![format!(
                    "  {} {} {} {} {} {}  {}{}",
                    rpad(&c.gen.to_string(), 3),
                    pad(&c.origin, ORIGIN_W),
                    rpad(&c.median_ns.map(dur).unwrap_or_else(|| "-".into()), 10),
                    rpad(&c.peak_rss_kb.map(mb).unwrap_or_else(|| "-".into()), 8),
                    rpad(&c.tokens.to_string(), 6),
                    rpad(&c.speedup.map(speedup).unwrap_or_else(|| "-".into()), 8),
                    decision,
                    detail
                )]
            }
            Event::Generation(g) => {
                let what = if g.evaluated == 0 {
                    "no new candidates".to_string()
                } else {
                    format!("{} evaluated, {} kept", g.evaluated, g.kept)
                };
                let elite = if g.new_elite {
                    format!("new elite {} at {}", g.best_origin, dur(g.best_median_ns))
                } else {
                    format!("elite unchanged ({})", g.best_origin)
                };
                vec![p.dim(format!("  ─── gen {}: {what} · {elite}", g.gen))]
            }
            Event::Note(n) => vec![format!("  {} {n}", p.yellow("note:"))],
            Event::Stop(s) => vec![p.dim(format!("  ─── stop: {s}"))],
            Event::ConfirmStart { origin, rounds } => vec![
                String::new(),
                format!(
                    "  {} {} against the baseline, {} interleaved runs in fresh sandboxes \
                     (hash seeds 1-{})",
                    p.bold("confirm"),
                    origin,
                    rounds,
                    rounds
                ),
            ],
            Event::Confirm(c) => {
                let verdict = if c.passed {
                    p.bold_green("✓ confirmed")
                } else {
                    p.bold_red(format!(
                        "✗ not confirmed: {}",
                        c.detail.clone().unwrap_or_default()
                    ))
                };
                vec![format!(
                    "          baseline {} · candidate {} · {} · won {}/{}  {}",
                    dur(c.baseline_median_ns),
                    dur(c.candidate_median_ns),
                    speedup(c.speedup),
                    c.candidate_wins,
                    c.baseline_times_ns.len(),
                    verdict
                )]
            }
            Event::Finished(s) => {
                let lines = summary_lines(&p, s, true);
                self.finished.push((**s).clone());
                lines
            }
        }
    }
}

/// The end-of-run block for one file. `with_diff` adds the winner's diff.
pub fn summary_lines(p: &Paint, s: &Summary, with_diff: bool) -> Vec<String> {
    let mut v = vec![String::new()];
    let label = |t: &str| p.dim(pad(t, 10));
    if s.confirmed {
        v.push(format!(
            "  {} {}  {} faster, confirmed",
            p.bold_green("✓"),
            p.bold(&s.file),
            p.bold_green(speedup(s.speedup))
        ));
        let rounds = s.rounds.unwrap_or(0);
        v.push(format!(
            "    {} {}  {}",
            label("baseline"),
            pad(&dur(s.confirm_baseline_ns.unwrap_or(s.baseline_ns)), 9),
            p.dim(format!(
                "median of {rounds} interleaved runs (search phase: {})",
                dur(s.baseline_ns)
            ))
        ));
        v.push(format!(
            "    {} {}  {}",
            label("optimized"),
            pad(&dur(s.best_ns), 9),
            s.best_origin
        ));
    } else {
        v.push(format!(
            "  {} {}  no faster version found, original kept",
            p.bold_yellow("–"),
            p.bold(&s.file)
        ));
        v.push(format!(
            "    {} {}  {}",
            label("baseline"),
            pad(&dur(s.baseline_ns), 9),
            p.dim("median, search phase")
        ));
        v.push(format!("    {} {}", label("reason"), s.reason));
    }
    v.push(format!(
        "    {} {} candidates, {} kept",
        label("searched"),
        s.evaluated,
        s.kept
    ));
    v.push(format!(
        "    {} {}",
        label("stopped"),
        p.dim(&s.stop_reason)
    ));
    for (i, path) in s.written.iter().enumerate() {
        let tag = if i == 0 { "wrote" } else { "" };
        let note = if i == 0 && !s.confirmed {
            p.dim("  (unchanged copy of the original)")
        } else {
            String::new()
        };
        v.push(format!(
            "    {} {}{note}",
            label(tag),
            p.cyan(path.display().to_string())
        ));
    }
    if with_diff && s.confirmed {
        let d = diff(&s.original, &s.best_source, 1);
        if !d.is_empty() {
            v.push(format!("    {}", label("change")));
            let max = 24;
            for (kind, text) in d.iter().take(max) {
                let line = match kind {
                    DiffKind::Hunk => p.cyan(text),
                    DiffKind::Added => p.green(text),
                    DiffKind::Removed => p.red(text),
                    DiffKind::Context => p.dim(text),
                };
                v.push(format!("      {line}"));
            }
            if d.len() > max {
                v.push(p.dim(format!(
                    "      … {} more diff lines in the .best.py",
                    d.len() - max
                )));
            }
        }
    }
    v
}

/// A table across files, printed when more than one file ran.
pub fn totals_lines(p: &Paint, all: &[Summary]) -> Vec<String> {
    let w = all
        .iter()
        .map(|s| s.file.chars().count())
        .max()
        .unwrap_or(4)
        .max(4);
    let mut v = vec![
        String::new(),
        p.dim(format!(
            "  {} {} {} {}  result",
            pad("file", w),
            rpad("baseline", 10),
            rpad("best", 10),
            rpad("speedup", 8)
        )),
    ];
    for s in all {
        let base = s
            .confirm_baseline_ns
            .filter(|_| s.confirmed)
            .unwrap_or(s.baseline_ns);
        let result = if s.confirmed {
            p.green("confirmed")
        } else {
            p.yellow("original kept")
        };
        v.push(format!(
            "  {} {} {} {}  {}",
            pad(&s.file, w),
            rpad(&dur(base), 10),
            rpad(&dur(s.best_ns), 10),
            rpad(&speedup(s.speedup), 8),
            result
        ));
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_pick_a_readable_unit() {
        assert_eq!(dur(317_726_000), "317.7 ms");
        assert_eq!(dur(1_345_000), "1.345 ms");
        assert_eq!(dur(806_000), "806.0 µs");
        assert_eq!(dur(950), "950 ns");
        assert_eq!(dur(2_500_000_000), "2.500 s");
    }

    #[test]
    fn margin_label_uses_the_real_margin() {
        assert_eq!(
            decision_text(Decision::RejectMargin, 0.05),
            "rejected: not 5% faster"
        );
        assert_eq!(
            decision_text(Decision::RejectMargin, 0.125),
            "rejected: not 12.5% faster"
        );
    }

    #[test]
    fn paint_off_adds_no_escapes() {
        assert_eq!(Paint::new(false).bold_red("x"), "x");
        assert_eq!(Paint::new(true).red("x"), "\x1b[31mx\x1b[0m");
    }

    #[test]
    fn never_and_always_ignore_the_terminal() {
        assert!(!color_enabled(ColorChoice::Never, true));
        assert!(color_enabled(ColorChoice::Always, false));
        assert!(!color_enabled(ColorChoice::Auto, false));
    }

    #[test]
    fn diff_shows_only_the_change_with_context() {
        let a = "a\nb\nc\nd\ne\nf\n";
        let b = "a\nb\nC\nd\ne\nf\n";
        let d = diff(a, b, 1);
        let text: Vec<&str> = d.iter().map(|(_, t)| t.as_str()).collect();
        assert_eq!(text, ["@@ -2,3 +2,3 @@", " b", "-c", "+C", " d"]);
        assert!(diff(a, a, 2).is_empty());
    }
}
