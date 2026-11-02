//! Evolutionary search with a measured reward.
//!
//! Each generation proposes rewrites of the current elite, evaluates them in
//! the sandbox with a small bounded pool, and keeps a new elite only when it
//! matches the oracle and its median is at least `margin` under the elite's.
//! Before reporting, the winner and the baseline are re-measured interleaved in
//! fresh sandboxes and must still clear the margin.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Instant;

use serde::Serialize;

use crate::policy;
use crate::propose::{Candidate, Proposer};
use crate::runner::{Outcome, Runner};
use crate::score::{self, judge, median, reward, same_behaviour, Decision, Oracle};

/// Baselines faster than this are not optimised: per-run noise in a fresh
/// sandbox is then too large relative to the time to trust a 5% gate.
pub const MIN_BASELINE_NS: u64 = 1_000_000;

/// Fraction of confirmation rounds the candidate must win head to head.
pub const PAIRED_WIN_FRACTION: f64 = 0.75;

/// A local file in the run (project mode).
#[derive(Clone, Debug, Default, Serialize)]
pub struct FileInfo {
    pub path: String,
    pub lines: usize,
    pub bytes: u64,
}

/// An installed requirement.
#[derive(Clone, Debug, Default, Serialize)]
pub struct PackageInfo {
    pub name: String,
    pub version: String,
}

/// What the run covers beyond the one file: filled in by the caller and
/// shown in the dashboard header and the report.
#[derive(Clone, Debug, Default, Serialize)]
pub struct RunInfo {
    /// Project root, when the target imports local modules.
    pub project_root: Option<String>,
    /// The script that runs (relative to the root).
    pub target: Option<String>,
    /// The file being rewritten (relative to the root).
    pub mutated: Option<String>,
    /// Every local file copied into the sandbox, target first.
    pub files: Vec<FileInfo>,
    /// Where requirements came from.
    pub requirements: Option<String>,
    pub packages: Vec<PackageInfo>,
    /// Script arguments after `--`.
    pub argv: Vec<String>,
    /// File fed to stdin.
    pub stdin: Option<String>,
    pub notes: Vec<String>,
}

/// Where the time went, in nanoseconds of wall clock.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Timings {
    /// Resolving the import closure (reading and parsing every local file).
    pub project_ns: u64,
    /// One parse of the file being rewritten.
    pub parse_ns: u64,
    /// The policy pre-filter over the rewritten file and every local module.
    pub policy_ns: u64,
    /// Installing or loading requirements (0 when there are none).
    pub deps_ns: u64,
    /// Time spent inside the proposer.
    pub propose_ns: u64,
    /// The whole search including sandbox runs and confirmation.
    pub search_ns: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct Config {
    pub gens: usize,
    pub pop: usize,
    pub patience: usize,
    pub reps: usize,
    pub margin: f64,
    pub jobs: usize,
    pub token_budget: u32,
    /// Baselines faster than this are reported as too fast to measure.
    pub min_baseline_ns: u64,
    /// Context shown in the header and copied into the report.
    #[serde(skip)]
    pub info: RunInfo,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            gens: 3,
            pop: 4,
            patience: 1,
            reps: 5,
            margin: 0.05,
            jobs: 2,
            token_budget: 512,
            min_baseline_ns: MIN_BASELINE_NS,
            info: RunInfo::default(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct CandidateRecord {
    pub gen: usize,
    pub index: usize,
    pub origin: String,
    pub tokens: u32,
    pub median_ns: Option<u64>,
    pub times_ns: Vec<u64>,
    pub peak_rss_kb: Option<u64>,
    pub speedup: Option<f64>,
    pub reward: Option<f64>,
    pub decision: Decision,
    pub detail: Option<String>,
    /// Unified diff of this candidate against the version it was derived from
    /// (at most [`CHANGE_LINES`] lines), so a rejection can be understood.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub change: Option<String>,
}

/// Longest candidate diff kept in the report.
pub const CHANGE_LINES: usize = 80;

/// A compact unified diff of `b` against `a` for the report.
fn change_text(a: &str, b: &str) -> Option<String> {
    // Each line already carries its unified-diff prefix.
    let lines = crate::ui::diff(a, b, 2);
    if lines.is_empty() {
        return None;
    }
    let total = lines.len();
    let mut out: Vec<String> = lines
        .into_iter()
        .take(CHANGE_LINES)
        .map(|(_, l)| l)
        .collect();
    if total > CHANGE_LINES {
        out.push(format!("… {} more diff lines", total - CHANGE_LINES));
    }
    Some(out.join("\n"))
}

#[derive(Clone, Debug, Serialize)]
pub struct GenerationRecord {
    pub gen: usize,
    pub evaluated: usize,
    pub kept: usize,
    pub new_elite: bool,
    pub best_origin: String,
    pub best_median_ns: u64,
    pub best_peak_rss_kb: u64,
    pub best_reward: f64,
    pub tokens: u32,
}

#[derive(Clone, Debug, Serialize)]
pub struct Confirmation {
    pub origin: String,
    pub rounds: usize,
    pub baseline_times_ns: Vec<u64>,
    pub candidate_times_ns: Vec<u64>,
    pub baseline_median_ns: u64,
    pub candidate_median_ns: u64,
    pub speedup: f64,
    /// Rounds in which the candidate was faster than the baseline.
    pub candidate_wins: usize,
    /// `PYTHONHASHSEED` of each round (baseline and candidate share it).
    pub seeds: Vec<u32>,
    pub oracle_ok: bool,
    pub passed: bool,
    pub detail: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Measured {
    pub median_ns: u64,
    pub times_ns: Vec<u64>,
    pub peak_rss_kb: u64,
    pub exit_code: i64,
    /// First 2000 characters of stderr, when there was any.
    pub stderr: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Best {
    pub origin: String,
    pub median_ns: u64,
    pub peak_rss_kb: u64,
    pub speedup: f64,
    pub confirmed: bool,
    pub reason: String,
    /// The confirmed version includes a model rewrite, which was checked on
    /// this run's input only and should be reviewed.
    pub model_rewrite: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct Report {
    /// Bumped when a field is renamed or removed.
    pub schema_version: u32,
    pub tool: String,
    pub file: String,
    pub sandbox: String,
    pub proposer: String,
    pub config: Config,
    /// Project files, requirements and packages, arguments.
    pub run: RunInfo,
    pub baseline: Measured,
    pub candidates: Vec<CandidateRecord>,
    pub generations: Vec<GenerationRecord>,
    pub stop_reason: String,
    pub confirmation: Option<Confirmation>,
    pub best: Best,
    pub best_source: String,
    pub notes: Vec<String>,
    pub timings: Timings,
}

/// Progress events, in order. The TUI and `--plain` render these; neither
/// re-parses the file.
#[derive(Clone, Debug)]
pub enum Event {
    Start {
        file: String,
        sandbox: String,
        proposer: String,
        config: Config,
    },
    /// A generation is about to evaluate `batch` candidates.
    GenStart {
        gen: usize,
        batch: usize,
    },
    Baseline(Measured),
    Candidate(CandidateRecord),
    Generation(GenerationRecord),
    Note(String),
    Stop(String),
    ConfirmStart {
        origin: String,
        rounds: usize,
    },
    /// One interleaved confirmation round (baseline and candidate) finished.
    ConfirmRound {
        done: usize,
        rounds: usize,
    },
    Confirm(Confirmation),
    Best(Best),
    /// The file is done and its outputs are on disk. Sent by the caller of
    /// [`optimize`] after writing them.
    Finished(Box<Summary>),
}

/// What the end-of-run summary shows for one file. Every number is copied from
/// the [`Report`]; nothing here is estimated.
#[derive(Clone, Debug)]
pub struct Summary {
    pub file: String,
    /// Baseline median from the search phase.
    pub baseline_ns: u64,
    /// Baseline median from the interleaved confirmation re-run, if one ran.
    pub confirm_baseline_ns: Option<u64>,
    pub best_origin: String,
    /// Winner's median from the confirmation re-run (or the baseline's).
    pub best_ns: u64,
    pub speedup: f64,
    pub confirmed: bool,
    pub reason: String,
    pub stop_reason: String,
    pub rounds: Option<usize>,
    pub evaluated: usize,
    pub kept: usize,
    pub original: String,
    pub best_source: String,
    pub written: Vec<std::path::PathBuf>,
    /// See [`Best::model_rewrite`].
    pub review: bool,
}

impl Summary {
    pub fn new(report: &Report, original: &str, written: Vec<std::path::PathBuf>) -> Self {
        Summary {
            file: report.file.clone(),
            baseline_ns: report.baseline.median_ns,
            confirm_baseline_ns: report.confirmation.as_ref().map(|c| c.baseline_median_ns),
            best_origin: report.best.origin.clone(),
            best_ns: report.best.median_ns,
            speedup: report.best.speedup,
            confirmed: report.best.confirmed,
            reason: report.best.reason.clone(),
            stop_reason: report.stop_reason.clone(),
            rounds: report.confirmation.as_ref().map(|c| c.rounds),
            evaluated: report.candidates.len(),
            kept: report
                .candidates
                .iter()
                .filter(|c| c.decision == Decision::Keep)
                .count(),
            original: original.to_string(),
            best_source: report.best_source.clone(),
            written,
            review: report.best.model_rewrite,
        }
    }
}

struct Elite {
    source: String,
    origin: String,
    outcome: Outcome,
    reward: f64,
}

/// Run `runner` over `sources` with at most `jobs` in flight, releasing results
/// to `on_done` in candidate order as soon as each prefix is complete.
pub fn evaluate(
    runner: &dyn Runner,
    sources: &[String],
    reps: usize,
    jobs: usize,
    mut on_done: impl FnMut(usize, Outcome),
) {
    let n = sources.len();
    if n == 0 {
        return;
    }
    let next = AtomicUsize::new(0);
    let (tx, rx) = mpsc::channel::<(usize, Outcome)>();
    std::thread::scope(|s| {
        for _ in 0..jobs.clamp(1, n) {
            let tx = tx.clone();
            let next = &next;
            s.spawn(move || loop {
                let i = next.fetch_add(1, Ordering::SeqCst);
                if i >= n {
                    break;
                }
                let out = runner.run(&sources[i], reps);
                if tx.send((i, out)).is_err() {
                    break;
                }
            });
        }
        drop(tx);
        let mut pending: Vec<Option<Outcome>> = vec![None; n];
        let mut cursor = 0;
        for (i, out) in rx {
            pending[i] = Some(out);
            while cursor < n {
                match pending[cursor].take() {
                    Some(o) => {
                        on_done(cursor, o);
                        cursor += 1;
                    }
                    None => break,
                }
            }
        }
    });
}

fn measured(o: &Outcome) -> Measured {
    Measured {
        median_ns: o.median_ns,
        times_ns: o.times_ns.clone(),
        peak_rss_kb: o.peak_rss_kb,
        exit_code: o.exit_code,
        stderr: (!o.stderr.is_empty()).then(|| o.stderr.chars().take(2000).collect()),
    }
}

/// Optimise one file.
#[allow(clippy::too_many_arguments)]
pub fn optimize(
    file: &str,
    source: &str,
    proposer: &dyn Proposer,
    runner: &dyn Runner,
    cfg: &Config,
    check: Option<&str>,
    cancel: &AtomicBool,
    emit: &mut dyn FnMut(Event),
) -> Result<Report, String> {
    let started = Instant::now();
    let mut propose_ns = 0u64;
    let mut notes: Vec<String> = Vec::new();
    policy::check(source).map_err(|v| format!("{file}: rejected before running: {v}"))?;
    emit(Event::Start {
        file: file.to_string(),
        sandbox: runner.name().to_string(),
        proposer: proposer.name(),
        config: cfg.clone(),
    });

    let baseline = runner.run(source, cfg.reps);
    if !baseline.ok() {
        let why = if baseline.timed_out {
            "timed out".to_string()
        } else {
            baseline
                .error
                .clone()
                .unwrap_or_else(|| "no timings".into())
        };
        return Err(format!("{file}: baseline failed in the sandbox: {why}"));
    }
    let oracle = match check {
        Some(expected) => {
            let o = Oracle::Expected {
                stdout: expected.to_string(),
                exit_code: baseline.exit_code,
            };
            if !o.matches(&baseline) {
                return Err(format!("{file}: baseline stdout does not match --check"));
            }
            o
        }
        None => Oracle::of(&baseline),
    };
    emit(Event::Baseline(measured(&baseline)));
    if baseline.exit_code != 0 {
        let n = format!(
            "the original exits with status {}; candidates must exit the same way",
            baseline.exit_code
        );
        emit(Event::Note(n.clone()));
        notes.push(n);
    }
    let too_fast = baseline.median_ns < cfg.min_baseline_ns;
    if too_fast {
        let n = format!(
            "too fast to measure reliably: the original runs in {} per rep, under the {} \
             floor where run-to-run noise rivals the {:.0}% margin; keeping the original",
            crate::ui::dur(baseline.median_ns),
            crate::ui::dur(cfg.min_baseline_ns),
            cfg.margin * 100.0
        );
        emit(Event::Note(n.clone()));
        notes.push(n);
    }

    let base_reward = reward(&baseline, &baseline, 0, cfg.token_budget).value;
    let mut elite = Elite {
        source: source.to_string(),
        origin: "baseline".into(),
        outcome: baseline.clone(),
        reward: base_reward,
    };
    let mut seen: HashSet<String> = HashSet::from([source.to_string()]);
    let mut records = Vec::new();
    let mut generations = Vec::new();
    let mut stale = 0usize;
    let mut stop_reason = format!("reached --gens {}", cfg.gens);
    let slots = cfg.pop.saturating_sub(1).max(1);
    if too_fast {
        stop_reason = format!(
            "too fast to measure: baseline median {} is under {}",
            crate::ui::dur(baseline.median_ns),
            crate::ui::dur(cfg.min_baseline_ns)
        );
    }

    for gen in (1..=cfg.gens).take_while(|_| !too_fast) {
        if cancel.load(Ordering::SeqCst) {
            stop_reason = "cancelled".into();
            break;
        }
        let t = Instant::now();
        let proposals = proposer.propose(&elite.source);
        propose_ns += t.elapsed().as_nanos() as u64;
        for note in proposer.take_notes() {
            emit(Event::Note(note.clone()));
            notes.push(note);
        }
        let mut batch: Vec<Candidate> = Vec::new();
        for cand in proposals {
            if batch.len() >= slots {
                break;
            }
            if !seen.insert(cand.source.clone()) {
                continue;
            }
            if let Err(v) = policy::check(&cand.source) {
                let rec = CandidateRecord {
                    gen,
                    index: records.len(),
                    origin: cand.origin,
                    tokens: cand.tokens,
                    median_ns: None,
                    times_ns: Vec::new(),
                    peak_rss_kb: None,
                    speedup: None,
                    reward: None,
                    decision: Decision::RejectPolicy,
                    detail: Some(v.to_string()),
                    change: change_text(&elite.source, &cand.source),
                };
                emit(Event::Candidate(rec.clone()));
                records.push(rec);
                continue;
            }
            batch.push(cand);
        }

        let sources: Vec<String> = batch.iter().map(|c| c.source.clone()).collect();
        emit(Event::GenStart {
            gen,
            batch: batch.len(),
        });
        let reference = elite.outcome.median_ns;
        let mut best_new: Option<(usize, f64, Outcome)> = None;
        let mut kept = 0;
        let tokens: u32 = batch.iter().map(|c| c.tokens).sum();
        evaluate(runner, &sources, cfg.reps, cfg.jobs, |i, out| {
            let cand = &batch[i];
            let decision = judge(reference, &oracle, &out, cfg.margin);
            let (speedup, rew) = if out.ok() {
                let r = reward(&baseline, &out, cand.tokens, cfg.token_budget);
                (Some(r.speedup), Some(r.value))
            } else {
                (None, None)
            };
            let detail = if out.timed_out {
                Some("wall-clock timeout".to_string())
            } else if out.error.is_some() {
                out.error.clone()
            } else {
                oracle.mismatch(&out).map(str::to_string)
            };
            let rec = CandidateRecord {
                gen,
                index: records.len(),
                origin: cand.origin.clone(),
                tokens: cand.tokens,
                median_ns: out.ok().then_some(out.median_ns),
                times_ns: out.times_ns.clone(),
                peak_rss_kb: out.ok().then_some(out.peak_rss_kb),
                speedup,
                reward: rew,
                decision,
                detail,
                change: change_text(&elite.source, &cand.source),
            };
            emit(Event::Candidate(rec.clone()));
            records.push(rec);
            if decision == Decision::Keep {
                kept += 1;
                let value = rew.unwrap_or(f64::MIN);
                if best_new.as_ref().is_none_or(|(_, v, _)| value > *v) {
                    best_new = Some((i, value, out));
                }
            }
        });

        let new_elite = best_new.is_some();
        if let Some((i, value, out)) = best_new {
            elite = Elite {
                source: batch[i].source.clone(),
                origin: batch[i].origin.clone(),
                outcome: out,
                reward: value,
            };
            stale = 0;
        } else {
            stale += 1;
        }
        let g = GenerationRecord {
            gen,
            evaluated: batch.len(),
            kept,
            new_elite,
            best_origin: elite.origin.clone(),
            best_median_ns: elite.outcome.median_ns,
            best_peak_rss_kb: elite.outcome.peak_rss_kb,
            best_reward: elite.reward,
            tokens,
        };
        emit(Event::Generation(g.clone()));
        generations.push(g);
        if cancel.load(Ordering::SeqCst) {
            stop_reason = "cancelled".into();
            break;
        }
        if batch.is_empty() && !new_elite && proposer.deterministic() {
            stop_reason =
                "search exhausted: the proposer has no new candidates for the elite".into();
            break;
        }
        if stale >= cfg.patience.max(1) && gen < cfg.gens {
            stop_reason = format!(
                "early stop: {stale} generation(s) without a new elite (--patience {})",
                cfg.patience.max(1)
            );
            break;
        }
    }
    emit(Event::Stop(stop_reason.clone()));

    let mut confirmation = None;
    let mut best = if too_fast {
        Best {
            origin: "baseline".into(),
            median_ns: baseline.median_ns,
            peak_rss_kb: baseline.peak_rss_kb,
            speedup: 1.0,
            confirmed: false,
            reason: "too fast to measure reliably; the original is kept".into(),
            model_rewrite: false,
        }
    } else if elite.origin == "baseline" {
        Best {
            origin: "baseline".into(),
            median_ns: baseline.median_ns,
            peak_rss_kb: baseline.peak_rss_kb,
            speedup: 1.0,
            confirmed: false,
            reason: "no candidate matched the oracle and cleared the time margin".into(),
            model_rewrite: false,
        }
    } else if cancel.load(Ordering::SeqCst) {
        Best {
            origin: "baseline".into(),
            median_ns: baseline.median_ns,
            peak_rss_kb: baseline.peak_rss_kb,
            speedup: 1.0,
            confirmed: false,
            reason: "cancelled before confirmation".into(),
            model_rewrite: false,
        }
    } else {
        let c = confirm(source, &elite, &oracle, runner, cfg, cancel, emit);
        let b = if c.passed {
            Best {
                origin: elite.origin.clone(),
                median_ns: c.candidate_median_ns,
                peak_rss_kb: elite.outcome.peak_rss_kb,
                speedup: c.speedup,
                confirmed: true,
                reason: format!(
                    "confirmed by an interleaved re-run ({}/{} rounds won, {} hash seeds)",
                    c.candidate_wins, c.rounds, c.rounds
                ),
                model_rewrite: false,
            }
        } else {
            Best {
                origin: "baseline".into(),
                median_ns: baseline.median_ns,
                peak_rss_kb: baseline.peak_rss_kb,
                speedup: 1.0,
                confirmed: false,
                reason: format!(
                    "{} failed confirmation: {}",
                    elite.origin,
                    c.detail.clone().unwrap_or_default()
                ),
                model_rewrite: false,
            }
        };
        confirmation = Some(c);
        b
    };
    if best.confirmed
        && generations
            .iter()
            .any(|g: &GenerationRecord| g.new_elite && g.best_origin.starts_with("model:"))
    {
        // Outputs are compared on this run's input only. A model can return a
        // program that is shorter because it is only right for that input
        // (for example, one that computes the known answer another way).
        let rounds = confirmation.as_ref().map_or(0, |c: &Confirmation| c.rounds);
        let n = format!(
            "the winning version includes a model rewrite: aco checked that it behaves the \
             same on this run's input (stdout, stderr and exit code, under {rounds} hash seeds), \
             not that it is equivalent for every input; review the diff before using it"
        );
        emit(Event::Note(n.clone()));
        notes.push(n);
        best.model_rewrite = true;
    }
    emit(Event::Best(best.clone()));
    let best_source = if best.confirmed {
        elite.source.clone()
    } else {
        source.to_string()
    };

    Ok(Report {
        schema_version: 1,
        tool: format!("aco {}", env!("CARGO_PKG_VERSION")),
        file: file.to_string(),
        sandbox: runner.name().to_string(),
        proposer: proposer.name(),
        config: cfg.clone(),
        run: cfg.info.clone(),
        baseline: measured(&baseline),
        candidates: records,
        generations,
        stop_reason,
        confirmation,
        best,
        best_source,
        notes,
        timings: Timings {
            propose_ns,
            search_ns: started.elapsed().as_nanos() as u64,
            ..Timings::default()
        },
    })
}

/// Re-measure baseline and elite interleaved (B, C, B, C, ...) in fresh
/// sandboxes, one timed rep each, for `2 * reps` rounds. Round `i` runs both
/// with `PYTHONHASHSEED=i+1`, and their stdout, stderr and exit code must
/// agree under every seed, which catches rewrites whose output depends on set
/// or dict-of-set iteration order. The candidate must also clear the margin
/// on the medians and win at least [`PAIRED_WIN_FRACTION`] of the rounds.
fn confirm(
    source: &str,
    elite: &Elite,
    oracle: &Oracle,
    runner: &dyn Runner,
    cfg: &Config,
    cancel: &AtomicBool,
    emit: &mut dyn FnMut(Event),
) -> Confirmation {
    let rounds = 2 * cfg.reps.max(1);
    emit(Event::ConfirmStart {
        origin: elite.origin.clone(),
        rounds,
    });
    let mut bt = Vec::with_capacity(rounds);
    let mut ct = Vec::with_capacity(rounds);
    let mut seeds = Vec::with_capacity(rounds);
    let mut oracle_ok = true;
    let mut detail = None;
    let mut wins = 0usize;
    for round in 0..rounds {
        if cancel.load(Ordering::SeqCst) {
            detail = Some("cancelled".to_string());
            break;
        }
        let seed = round as u32 + 1;
        let b = runner.run_seeded(source, 1, seed);
        let c = runner.run_seeded(&elite.source, 1, seed);
        if !b.ok() || !c.ok() {
            detail = Some(format!(
                "a confirmation run failed (baseline: {:?}, candidate: {:?})",
                b.error, c.error
            ));
            break;
        }
        // Same behaviour as the baseline under this seed; with --check the
        // candidate must also still match the expected output.
        let agrees = same_behaviour(&b, &c)
            && (matches!(oracle, Oracle::Baseline { .. }) || oracle.matches(&c));
        if !agrees && oracle_ok {
            detail = Some(format!(
                "output differed from the original with PYTHONHASHSEED={seed} (it depends on \
                 hash or iteration order)"
            ));
        }
        oracle_ok &= agrees;
        seeds.push(seed);
        if c.times_ns[0] < b.times_ns[0] {
            wins += 1;
        }
        bt.push(b.times_ns[0]);
        ct.push(c.times_ns[0]);
        emit(Event::ConfirmRound {
            done: round + 1,
            rounds,
        });
    }
    let bm = median(&bt);
    let cm = median(&ct);
    let complete = bt.len() == rounds;
    let need = (PAIRED_WIN_FRACTION * rounds as f64).ceil() as usize;
    let fast = complete && score::clears_margin(bm, cm, cfg.margin);
    let consistent = wins >= need;
    if complete && oracle_ok && !fast {
        detail = Some(format!(
            "median {} vs baseline {} is under the {:.0}% margin",
            crate::ui::dur(cm),
            crate::ui::dur(bm),
            cfg.margin * 100.0
        ));
    } else if complete && oracle_ok && !consistent {
        detail = Some(format!(
            "faster in only {wins} of {rounds} paired rounds (needs {need}); the difference \
             is within run-to-run noise"
        ));
    }
    let c = Confirmation {
        origin: elite.origin.clone(),
        rounds,
        baseline_times_ns: bt,
        candidate_times_ns: ct,
        baseline_median_ns: bm,
        candidate_median_ns: cm,
        speedup: if cm > 0 { bm as f64 / cm as f64 } else { 0.0 },
        candidate_wins: wins,
        seeds,
        oracle_ok,
        passed: complete && oracle_ok && fast && consistent,
        detail,
    };
    emit(Event::Confirm(c.clone()));
    c
}
