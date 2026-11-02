use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use clap::{Parser, ValueEnum};

use aco::propose::http::{HttpProposer, DEFAULT_ENDPOINT, DEFAULT_MODEL};
use aco::propose::{Combined, Proposer, RuleProposer};
use aco::runner::{select, Limits, Runner, SandboxKind};
use aco::search::{optimize, Config, Event, Report};
use aco::tui::{plain_line, run_dashboard};

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum ProposerKind {
    /// Deterministic AST rules (no tokens).
    Rules,
    /// A local OpenAI-compatible server such as Ollama or llama.cpp.
    Ollama,
    /// Rules first, then the local model.
    Both,
}

/// Searches for faster Python by measuring real sandboxed runs.
///
/// Evolutionary search with a measured reward, not a trained policy. A
/// candidate is kept only if its stdout matches and its in-sandbox median is at
/// least --margin faster, and the winner must repeat that in an interleaved
/// re-run before it is reported.
#[derive(Parser, Debug)]
#[command(name = "aco", version)]
struct Args {
    /// Python files to optimise.
    #[arg(required = true)]
    files: Vec<PathBuf>,
    /// Maximum generations.
    #[arg(long, default_value_t = 3)]
    gens: usize,
    /// Population size per generation, including the elite.
    #[arg(long, default_value_t = 4)]
    pop: usize,
    /// Stop after this many generations without a new elite.
    #[arg(long, default_value_t = 1)]
    patience: usize,
    /// Timed reps per candidate (odd, scored by the median).
    #[arg(long, default_value_t = 5, value_parser = odd)]
    reps: usize,
    /// Untimed warmup runs per sandbox.
    #[arg(long, default_value_t = 1)]
    warmup: usize,
    /// Candidates evaluated in parallel.
    #[arg(long, default_value_t = 2)]
    jobs: usize,
    /// Required improvement over the elite's median.
    #[arg(long, default_value_t = 0.05)]
    margin: f64,
    #[arg(long, value_enum, default_value_t = ProposerKind::Rules)]
    proposer: ProposerKind,
    /// OpenAI-compatible server for --proposer ollama.
    #[arg(long, default_value = DEFAULT_ENDPOINT)]
    endpoint: String,
    /// Model name for --proposer ollama (qwen2.5-coder:3b is the larger local option).
    #[arg(long, default_value = DEFAULT_MODEL)]
    model: String,
    /// Response cap per model proposal, and the token-ratio denominator.
    #[arg(long, default_value_t = 512)]
    token_budget: u32,
    #[arg(long, value_enum, default_value_t = SandboxKind::Auto)]
    sandbox: SandboxKind,
    /// Wall-clock limit per sandbox run, in seconds.
    #[arg(long, default_value_t = 60)]
    timeout: u64,
    /// Memory limit per sandbox, in MiB.
    #[arg(long, default_value_t = 1024)]
    memory_mb: u64,
    /// Expected stdout file; candidates must match it exactly (one input file only).
    #[arg(long)]
    check: Option<PathBuf>,
    /// Where the best source (and --json report) are written.
    #[arg(long, default_value = "aco-out")]
    out: PathBuf,
    /// Print events as lines instead of the live dashboard.
    #[arg(long)]
    plain: bool,
    /// Write a JSON report next to the best source.
    #[arg(long)]
    json: bool,
}

fn odd(s: &str) -> Result<usize, String> {
    let n: usize = s.parse().map_err(|e| format!("{e}"))?;
    if n % 2 == 1 {
        Ok(n)
    } else {
        Err("must be odd so the median is a real sample".into())
    }
}

fn write_outputs(args: &Args, path: &Path, report: &Report) -> std::io::Result<Vec<PathBuf>> {
    std::fs::create_dir_all(&args.out)?;
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("candidate");
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
        let label = path.to_string_lossy().to_string();
        match optimize(&label, source, proposer, runner, &cfg, check, cancel, emit) {
            Ok(report) => match write_outputs(args, path, &report) {
                Ok(paths) => {
                    for p in paths {
                        emit(Event::Note(format!("wrote {}", p.display())));
                    }
                }
                Err(e) => errors.push(format!("{label}: could not write output: {e}")),
            },
            Err(e) => errors.push(e),
        }
    }
    errors
}

fn main() -> ExitCode {
    let args = Args::parse();
    if args.check.is_some() && args.files.len() != 1 {
        eprintln!("aco: --check takes exactly one input file");
        return ExitCode::from(2);
    }
    let mut inputs = Vec::new();
    for f in &args.files {
        match std::fs::read_to_string(f) {
            Ok(s) => inputs.push((f.clone(), s)),
            Err(e) => {
                eprintln!("aco: cannot read {}: {e}", f.display());
                return ExitCode::from(2);
            }
        }
    }
    // Reject out-of-policy inputs before any process starts.
    for (f, s) in &inputs {
        if let Err(v) = aco::policy::check(s) {
            eprintln!("aco: {} rejected before running: {v}", f.display());
            return ExitCode::from(2);
        }
    }
    let check = match &args.check {
        Some(p) => match std::fs::read_to_string(p) {
            Ok(s) => Some(s),
            Err(e) => {
                eprintln!("aco: cannot read --check {}: {e}", p.display());
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
            eprintln!("aco: {e}");
            return ExitCode::from(3);
        }
    };
    let samples = args.pop.saturating_sub(1).max(1);
    let model = || -> Box<dyn Proposer> {
        Box::new(HttpProposer::new(&args.endpoint, &args.model, args.token_budget, samples))
    };
    let proposer: Box<dyn Proposer> = match args.proposer {
        ProposerKind::Rules => Box::new(RuleProposer),
        ProposerKind::Ollama => model(),
        ProposerKind::Both => Box::new(Combined(vec![Box::new(RuleProposer), model()])),
    };

    let cancel = AtomicBool::new(false);
    let plain = args.plain || !std::io::stdout().is_terminal();
    let errors = if plain {
        let mut out = std::io::stdout().lock();
        let mut emit = |e: Event| {
            let _ = writeln!(out, "{}", plain_line(&e));
            let _ = out.flush();
        };
        run_all(&args, &inputs, check.as_deref(), proposer.as_ref(), runner.as_ref(), &cancel, &mut emit)
    } else {
        match run_dashboard(&cancel, |emit| {
            run_all(&args, &inputs, check.as_deref(), proposer.as_ref(), runner.as_ref(), &cancel, emit)
        }) {
            Ok(e) => e,
            Err(e) => vec![format!("terminal error: {e}")],
        }
    };
    for e in &errors {
        eprintln!("aco: {e}");
    }
    if errors.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}
