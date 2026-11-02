use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use clap::builder::styling::{AnsiColor, Effects, Styles};
use clap::{CommandFactory, FromArgMatches, Parser, ValueEnum};

use aco::propose::http::{HttpProposer, DEFAULT_ENDPOINT, DEFAULT_MODEL};
use aco::propose::{Combined, Proposer, RuleProposer};
use aco::runner::{abort_all, select, Limits, Runner, SandboxKind};
use aco::search::{optimize, Config, Event, Report, Summary};
use aco::tui::{run_dashboard, Exit};
use aco::ui::{self, ColorChoice, Paint, Plain};

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum ProposerKind {
    /// Deterministic AST rewrite rules (no model, no tokens)
    Rules,
    /// A local OpenAI-compatible server such as Ollama or llama.cpp
    Ollama,
    /// Rules first, then the local model
    Both,
}

const STYLES: Styles = Styles::styled()
    .header(AnsiColor::Green.on_default().effects(Effects::BOLD))
    .usage(AnsiColor::Green.on_default().effects(Effects::BOLD))
    .literal(AnsiColor::Cyan.on_default().effects(Effects::BOLD))
    .placeholder(AnsiColor::Cyan.on_default())
    .valid(AnsiColor::Cyan.on_default())
    .invalid(AnsiColor::Yellow.on_default())
    .error(AnsiColor::Red.on_default().effects(Effects::BOLD));

const EXAMPLES: &str = "\
\x1b[1;32mExamples:\x1b[0m
  aco slow.py                          Live dashboard; rewrites from the built-in rules
  aco --plain fixtures/*.py            Line output for logs and CI (also used when piped)
  aco --json --out results slow.py     Also write results/slow.report.json
  aco --check expected.txt slow.py     Require stdout to equal a file, byte for byte
  aco --proposer both slow.py          Rules plus a local model served by Ollama
  aco --gens 5 --patience 2 slow.py    Search longer before giving up

\x1b[1;32mOutputs:\x1b[0m
  <out>/<name>.best.py is the fastest confirmed version, or a copy of the original
  if nothing beat it. With --json, <out>/<name>.report.json has every measurement.

\x1b[1;32mExit status:\x1b[0m
  0 done · 1 a file failed in the sandbox · 2 bad input or usage · 3 no sandbox · 130 cancelled";

fn examples() -> String {
    // Keep the headings bold green like clap's, unless colour is off.
    if ui::stdout_color(ColorChoice::Auto) {
        EXAMPLES.to_string()
    } else {
        EXAMPLES.replace("\x1b[1;32m", "").replace("\x1b[0m", "")
    }
}

/// Find a faster version of a Python script by measuring real sandboxed runs.
///
/// aco proposes rewrites, runs every candidate in an isolated sandbox (Docker,
/// or Linux namespaces), and keeps one only if its stdout is identical and its
/// median time is at least --margin faster. The winner is then re-run against
/// the original, interleaved, and reported only if it holds up.
#[derive(Parser, Debug)]
#[command(name = "aco", version, styles = STYLES, max_term_width = 100)]
struct Args {
    /// Python files to optimise
    #[arg(required = true, value_name = "FILE")]
    files: Vec<PathBuf>,

    /// Maximum generations
    #[arg(long, default_value_t = 3, value_parser = at_least_1, help_heading = "Search")]
    gens: usize,
    /// Candidates per generation, including the current best
    #[arg(long, default_value_t = 4, value_parser = at_least_2, help_heading = "Search")]
    pop: usize,
    /// Stop after this many generations without a new best
    #[arg(long, default_value_t = 1, value_parser = at_least_1, help_heading = "Search")]
    patience: usize,
    /// Timed runs per candidate, scored by the median (odd)
    #[arg(long, default_value_t = 5, value_parser = odd, help_heading = "Search")]
    reps: usize,
    /// Untimed warmup runs before the timed ones
    #[arg(long, default_value_t = 1, help_heading = "Search")]
    warmup: usize,
    /// Required speedup over the current best, as a fraction (0.05 = 5%)
    #[arg(long, default_value_t = 0.05, value_parser = margin, help_heading = "Search")]
    margin: f64,
    /// Candidates measured in parallel
    #[arg(long, default_value_t = 2, value_parser = at_least_1, help_heading = "Search")]
    jobs: usize,
    /// Expected stdout file; candidates must match it exactly (one input file only)
    #[arg(long, value_name = "FILE", help_heading = "Search")]
    check: Option<PathBuf>,

    /// Where rewrites come from
    #[arg(long, value_enum, default_value_t = ProposerKind::Rules, help_heading = "Proposer")]
    proposer: ProposerKind,
    /// OpenAI-compatible server for the model proposer
    #[arg(long, default_value = DEFAULT_ENDPOINT, value_name = "URL", help_heading = "Proposer")]
    endpoint: String,
    /// Model name for the model proposer
    #[arg(long, default_value = DEFAULT_MODEL, help_heading = "Proposer")]
    model: String,
    /// Max response tokens per model proposal
    #[arg(
        long,
        default_value_t = 512,
        value_name = "N",
        help_heading = "Proposer"
    )]
    token_budget: u32,

    /// Isolation for candidate runs; never falls back to running unsandboxed
    #[arg(long, value_enum, default_value_t = SandboxKind::Auto, help_heading = "Sandbox")]
    sandbox: SandboxKind,
    /// Wall-clock limit per sandbox run, in seconds
    #[arg(long, default_value_t = 60, value_name = "SECS", value_parser = secs, help_heading = "Sandbox")]
    timeout: u64,
    /// Memory limit per sandbox run, in MiB
    #[arg(long, default_value_t = 1024, value_name = "MIB", value_parser = mib, help_heading = "Sandbox")]
    memory_mb: u64,

    /// Directory for <name>.best.py and the report
    #[arg(
        short,
        long,
        default_value = "aco-out",
        value_name = "DIR",
        help_heading = "Output"
    )]
    out: PathBuf,
    /// Also write <name>.report.json with every measurement
    #[arg(long, help_heading = "Output")]
    json: bool,
    /// Print line output instead of the live dashboard
    #[arg(long, help_heading = "Output")]
    plain: bool,
    /// When to use colour (auto honours NO_COLOR and non-terminal output)
    #[arg(long, value_enum, default_value_t = ColorChoice::Auto, value_name = "WHEN", help_heading = "Output")]
    color: ColorChoice,
}

fn parse_usize(s: &str) -> Result<usize, String> {
    s.parse()
        .map_err(|_| format!("`{s}` is not a whole number"))
}

fn at_least_1(s: &str) -> Result<usize, String> {
    let n = parse_usize(s)?;
    if n >= 1 {
        Ok(n)
    } else {
        Err("must be at least 1".into())
    }
}

fn at_least_2(s: &str) -> Result<usize, String> {
    let n = parse_usize(s)?;
    if n >= 2 {
        Ok(n)
    } else {
        Err("must be at least 2 (the current best plus one candidate)".into())
    }
}

fn odd(s: &str) -> Result<usize, String> {
    let n = parse_usize(s)?;
    if n % 2 == 1 {
        Ok(n)
    } else {
        Err("must be odd so the median is a real measurement".into())
    }
}

fn margin(s: &str) -> Result<f64, String> {
    let m: f64 = s.parse().map_err(|_| format!("`{s}` is not a number"))?;
    if (0.0..1.0).contains(&m) {
        Ok(m)
    } else {
        Err("must be a fraction from 0 up to (not including) 1, e.g. 0.05 for 5%".into())
    }
}

fn secs(s: &str) -> Result<u64, String> {
    let n: u64 = s
        .parse()
        .map_err(|_| format!("`{s}` is not a whole number"))?;
    if (1..=86_400).contains(&n) {
        Ok(n)
    } else {
        Err("must be between 1 and 86400 seconds".into())
    }
}

fn mib(s: &str) -> Result<u64, String> {
    let n: u64 = s
        .parse()
        .map_err(|_| format!("`{s}` is not a whole number"))?;
    if n >= 32 {
        Ok(n)
    } else {
        Err("must be at least 32 MiB (Python itself needs ~10)".into())
    }
}

/// Errors and hints on stderr, styled like cargo's.
struct Diag {
    p: Paint,
}

impl Diag {
    fn error(&self, msg: impl AsRef<str>) {
        eprintln!("{}: {}", self.p.bold_red("error"), msg.as_ref());
    }
    fn detail(&self, msg: impl AsRef<str>) {
        eprintln!("{}", msg.as_ref());
    }
    fn hint(&self, msg: impl AsRef<str>) {
        eprintln!("  {}: {}", self.p.bold_cyan("hint"), msg.as_ref());
    }
}

fn write_outputs(args: &Args, path: &Path, report: &Report) -> std::io::Result<Vec<PathBuf>> {
    std::fs::create_dir_all(&args.out)?;
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("candidate");
    let best = args.out.join(format!("{stem}.best.py"));
    std::fs::write(&best, &report.best_source)?;
    let mut written = vec![best];
    if args.json {
        let json = args.out.join(format!("{stem}.report.json"));
        let text = serde_json::to_string_pretty(report).map_err(std::io::Error::other)?;
        std::fs::write(&json, text + "\n")?;
        written.push(json);
    }
    Ok(written)
}

#[allow(clippy::too_many_arguments)]
fn run_all(
    args: &Args,
    inputs: &[(PathBuf, String)],
    check: Option<&str>,
    proposer: &dyn Proposer,
    runner: &dyn Runner,
    cancel: &AtomicBool,
    emit: &mut dyn FnMut(Event),
) -> Vec<String> {
    let cfg = Config {
        gens: args.gens,
        pop: args.pop,
        patience: args.patience,
        reps: args.reps,
        margin: args.margin,
        jobs: args.jobs,
        token_budget: args.token_budget,
    };
    let mut errors = Vec::new();
    for (path, source) in inputs {
        if cancel.load(Ordering::SeqCst) {
            break;
        }
        if std::env::var_os("ACO_TEST_PANIC").is_some() {
            // Test hook: proves the terminal is restored if the search panics.
            panic!("ACO_TEST_PANIC is set");
        }
        let label = path.to_string_lossy().to_string();
        match optimize(&label, source, proposer, runner, &cfg, check, cancel, emit) {
            Ok(report) => match write_outputs(args, path, &report) {
                Ok(paths) => emit(Event::Finished(Box::new(Summary::new(
                    &report, source, paths,
                )))),
                Err(e) => errors.push(format!(
                    "{label}: could not write to {}: {e}",
                    args.out.display()
                )),
            },
            Err(e) if cancel.load(Ordering::SeqCst) => {
                let _ = e;
                break;
            }
            Err(e) => errors.push(e),
        }
    }
    errors
}

/// Read and validate one input file, printing a diagnostic on failure.
fn load(d: &Diag, path: &Path) -> Option<String> {
    let shown = path.display();
    let text = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) => {
            match e.kind() {
                std::io::ErrorKind::NotFound => {
                    d.error(format!("`{shown}` does not exist"));
                    d.hint("check the path; aco takes Python source files");
                }
                std::io::ErrorKind::IsADirectory => {
                    d.error(format!("`{shown}` is a directory"));
                    d.hint(format!("pass the files inside it, e.g. `aco {shown}/*.py`"));
                }
                std::io::ErrorKind::PermissionDenied => {
                    d.error(format!("`{shown}` is not readable (permission denied)"));
                }
                _ => d.error(format!("cannot read `{shown}`: {e}")),
            }
            return None;
        }
    };
    let Ok(source) = String::from_utf8(text) else {
        d.error(format!("`{shown}` is not UTF-8 text"));
        d.hint("aco reads Python source files; is this a binary or another encoding?");
        return None;
    };
    if let Some(se) = aco::policy::syntax_error(&source) {
        d.error(format!("`{shown}` is not valid Python: {}", se.message));
        let gutter = se.line.to_string().len();
        d.detail(format!(
            "{}{} {shown}:{}:{}",
            " ".repeat(gutter),
            d.p.bold_cyan("-->"),
            se.line,
            se.column
        ));
        d.detail(format!("{} {}", " ".repeat(gutter), d.p.bold_cyan("|")));
        // Keep long lines readable: show a window around the column.
        let chars: Vec<char> = se.text.chars().collect();
        let start = se.column.saturating_sub(1).saturating_sub(60);
        let end = (start + 100).min(chars.len());
        let mut text: String = chars[start..end].iter().collect();
        if start > 0 {
            text = format!("…{text}");
        }
        if end < chars.len() {
            text.push('…');
        }
        let caret_at = se.column.saturating_sub(1) - start + usize::from(start > 0);
        d.detail(format!(
            "{} {} {}",
            d.p.bold_cyan(se.line.to_string()),
            d.p.bold_cyan("|"),
            text
        ));
        d.detail(format!(
            "{} {} {}{}",
            " ".repeat(gutter),
            d.p.bold_cyan("|"),
            " ".repeat(caret_at),
            d.p.bold_red("^")
        ));
        if path.extension().and_then(|e| e.to_str()) != Some("py") {
            d.hint("aco optimises Python source files (.py)");
        }
        return None;
    }
    if aco::policy::is_empty_module(&source) {
        d.error(format!(
            "`{shown}` has no code to optimise (it is empty or only comments)"
        ));
        return None;
    }
    if let Err(v) = aco::policy::check(&source) {
        d.error(format!("`{shown}` was rejected before running: it {v}"));
        d.hint(
            "aco only optimises self-contained computation: no file, process, network or \
             introspection access. The sandbox is the real boundary; this check just refuses \
             obvious cases early.",
        );
        return None;
    }
    Some(source)
}

fn main() -> ExitCode {
    let matches = Args::command().after_help(examples()).get_matches();
    let args = match Args::from_arg_matches(&matches) {
        Ok(a) => a,
        Err(e) => e.exit(),
    };
    let d = Diag {
        p: Paint::new(ui::stderr_color(args.color)),
    };

    if args.check.is_some() && args.files.len() != 1 {
        d.error("--check takes exactly one input file");
        return ExitCode::from(2);
    }
    let mut inputs = Vec::new();
    for f in &args.files {
        match load(&d, f) {
            Some(s) => inputs.push((f.clone(), s)),
            None => return ExitCode::from(2),
        }
    }
    let check = match &args.check {
        Some(p) => match std::fs::read_to_string(p) {
            Ok(s) => Some(s),
            Err(e) => {
                d.error(format!("cannot read --check file `{}`: {e}", p.display()));
                return ExitCode::from(2);
            }
        },
        None => None,
    };
    let limits = Limits {
        timeout: Duration::from_secs(args.timeout),
        memory_mb: args.memory_mb,
        warmup: args.warmup,
        ..Limits::default()
    };
    let runner = match select(args.sandbox, limits) {
        Ok(r) => r,
        Err(e) => {
            d.error("no sandbox is available, so aco will not run any code");
            for line in e.lines() {
                d.detail(format!("  {}", line.trim_start()));
            }
            d.hint("install Docker (and start it), or on Linux enable unprivileged user namespaces; see SECURITY.md");
            return ExitCode::from(3);
        }
    };
    let samples = args.pop.saturating_sub(1).max(1);
    let model = || -> Box<dyn Proposer> {
        Box::new(HttpProposer::new(
            &args.endpoint,
            &args.model,
            args.token_budget,
            samples,
        ))
    };
    let proposer: Box<dyn Proposer> = match args.proposer {
        ProposerKind::Rules => Box::new(RuleProposer),
        ProposerKind::Ollama => model(),
        ProposerKind::Both => Box::new(Combined(vec![Box::new(RuleProposer), model()])),
    };

    // SIGINT/SIGTERM/SIGHUP: stop the search and kill in-flight sandboxes, so
    // nothing outlives aco and the terminal is restored on the way out.
    let cancel = Arc::new(AtomicBool::new(false));
    let signalled = Arc::new(AtomicBool::new(false));
    let plain = args.plain || !std::io::stdout().is_terminal();
    for sig in [
        signal_hook::consts::SIGINT,
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGHUP,
    ] {
        let _ = signal_hook::flag::register(sig, signalled.clone());
    }
    {
        // First signal: cancel and kill sandboxes; the run winds down and
        // reports. A second signal: force quit right away.
        let (cancel, signalled) = (cancel.clone(), signalled.clone());
        std::thread::spawn(move || {
            let mut seen = 0;
            loop {
                if signalled.swap(false, Ordering::SeqCst) {
                    seen += 1;
                    cancel.store(true, Ordering::SeqCst);
                    abort_all();
                    if seen >= 2 {
                        force_quit(!plain);
                    }
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        });
    }

    let out_color = ui::stdout_color(args.color);
    let mut finished: Vec<Summary> = Vec::new();
    let errors = if plain {
        let mut r = Plain::new(out_color);
        let mut out = std::io::stdout().lock();
        let errors = {
            let mut emit = |e: Event| {
                for line in r.render(&e) {
                    let _ = writeln!(out, "{line}");
                }
                let _ = out.flush();
            };
            run_all(
                &args,
                &inputs,
                check.as_deref(),
                proposer.as_ref(),
                runner.as_ref(),
                &cancel,
                &mut emit,
            )
        };
        if r.finished.len() > 1 {
            for line in ui::totals_lines(&r.p, &r.finished) {
                let _ = writeln!(out, "{line}");
            }
        }
        finished = std::mem::take(&mut r.finished);
        errors
    } else {
        let on_cancel = || {
            // A second q/Ctrl-C while already cancelling forces the exit (a
            // slow model request cannot be interrupted).
            if aco::runner::aborted() {
                force_quit(true);
            }
            abort_all();
        };
        let res = run_dashboard(inputs.len(), &cancel, &on_cancel, |emit| {
            let mut local: Vec<Summary> = Vec::new();
            let errors = {
                let mut tap = |e: Event| {
                    if let Event::Finished(s) = &e {
                        local.push((**s).clone());
                    }
                    emit(e);
                };
                run_all(
                    &args,
                    &inputs,
                    check.as_deref(),
                    proposer.as_ref(),
                    runner.as_ref(),
                    &cancel,
                    &mut tap,
                )
            };
            (errors, local)
        });
        match res {
            Ok(Exit::Done((errors, local))) => {
                finished = local;
                errors
            }
            Ok(Exit::Panicked(msg)) => {
                abort_all();
                d.error(format!("internal error, the search stopped: {msg}"));
                d.hint("please report this at https://github.com/ayaan-cs/aco/issues");
                return ExitCode::from(101);
            }
            Err(e) => vec![format!("terminal error: {e}")],
        }
    };

    // After the dashboard closes, leave the result in the scrollback.
    if !plain {
        let p = Paint::new(out_color);
        let mut out = std::io::stdout().lock();
        for s in &finished {
            for line in ui::summary_lines(&p, s, false) {
                let _ = writeln!(out, "{line}");
            }
        }
        if finished.len() > 1 {
            for line in ui::totals_lines(&p, &finished) {
                let _ = writeln!(out, "{line}");
            }
        }
    }
    if cancel.load(Ordering::SeqCst) {
        d.error(format!(
            "cancelled; {} of {} file(s) finished",
            finished.len(),
            inputs.len()
        ));
        return ExitCode::from(130);
    }
    for e in &errors {
        d.error(e);
    }
    if errors.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// Exit now with 130 after giving the runner threads a moment to kill their
/// sandboxes (they poll the abort flag every few milliseconds).
fn force_quit(tui: bool) -> ! {
    abort_all();
    std::thread::sleep(Duration::from_millis(150));
    if tui {
        ratatui::restore();
    }
    eprintln!("aco: force quit");
    std::process::exit(130);
}
