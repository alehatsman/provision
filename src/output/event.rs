//! One event type that `plan` and `apply` both emit. Spec §9.
//!
//! The output layer knows nothing about actions: it is handed a name, a
//! status and a duration, and its whole job is to render them three ways.

use std::path::PathBuf;
use std::time::Duration;

/// The five verdicts of §9.1 plus the three `plan` uses. `Unknown` is a
/// first-class answer, not a failure: a `shell` step with no gate ran, and
/// nothing here can honestly say what it did (D3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Status {
    Ok,
    Changed,
    Unknown,
    Skipped(String),
    Failed(Failure),
    /// plan: the target was probed and differs. The plan-time twin of
    /// `Changed`, and it carries the same glyph and color.
    WouldChange,
    /// plan: a gate was probed and says the step has work to do.
    WouldRun,
    /// plan: nothing was probed, so no verdict is claimed (D11, D14).
    WouldRunUnprobed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Failure {
    pub msg: String,
    pub rc: Option<i32>,
    pub stderr: String,
    /// Ctrl-C, not the step's own fault. Changes the exit code, not the glyph.
    pub interrupted: bool,
}

impl Status {
    pub(crate) fn label(&self) -> String {
        match self {
            Status::Ok => "ok".into(),
            Status::Changed => "changed".into(),
            Status::Unknown => "unknown".into(),
            Status::Skipped(why) => format!("skipped   {why}"),
            Status::Failed(_) => "FAILED".into(),
            Status::WouldChange => "would change".into(),
            Status::WouldRun => "would run".into(),
            Status::WouldRunUnprobed => "would run (unprobed)".into(),
        }
    }

    pub(crate) fn glyph(&self) -> &'static str {
        match self {
            Status::Ok => "✓",
            Status::Changed => "~",
            Status::Unknown => "?",
            Status::Skipped(_) => "-",
            Status::Failed(_) => "✗",
            Status::WouldChange => "~",
            Status::WouldRun => "→",
            Status::WouldRunUnprobed => "?",
        }
    }

    pub(crate) fn failed(&self) -> bool {
        matches!(self, Status::Failed(_))
    }

    /// The word `--json` uses. Stable; the terminal wording is not.
    pub(crate) fn key(&self) -> &'static str {
        match self {
            Status::Ok => "ok",
            Status::Changed => "changed",
            Status::Unknown => "unknown",
            Status::Skipped(_) => "skipped",
            Status::Failed(_) => "failed",
            Status::WouldChange => "would_change",
            Status::WouldRun => "would_run",
            Status::WouldRunUnprobed => "would_run_unprobed",
        }
    }
}

/// One finished step.
#[derive(Debug, Clone)]
pub(crate) struct Event {
    pub index: usize,
    pub name: String,
    pub file: PathBuf,
    pub line: usize,
    pub depth: usize,
    pub status: Status,
    pub duration: Duration,
    /// `2` when the step succeeded on its second try. 1 for everything else.
    pub attempt: u32,
    pub attempts: u32,
    /// Captured output, kept so `--verbose` and the failure block can show it
    /// without the runner having to guess in advance which will be wanted.
    pub stdout: String,
    pub stderr: String,
    /// A diff, or a metadata delta like `mode 0644 → 0600`. Printed under the
    /// step's line, and carried in `--json` as `diff`.
    pub detail: Option<String>,
    /// Extra words for the status column: `3 of 14`. Spec §6.4 wants the
    /// count on the line, not buried in the diff underneath it.
    pub note: Option<String>,
}

#[derive(Debug, Default, Clone)]
pub(crate) struct Summary {
    pub total: usize,
    pub ok: usize,
    pub changed: usize,
    pub skipped: usize,
    pub unknown: usize,
    pub failed: usize,
    pub would_change: usize,
    pub would_run: usize,
    pub unprobed: usize,
    pub duration: Duration,
    pub interrupted: bool,
}

impl Summary {
    pub(crate) fn count(&mut self, s: &Status) {
        self.total += 1;
        match s {
            Status::Ok => self.ok += 1,
            Status::Changed => self.changed += 1,
            Status::Unknown => self.unknown += 1,
            Status::Skipped(_) => self.skipped += 1,
            Status::Failed(f) => {
                self.failed += 1;
                self.interrupted |= f.interrupted;
            }
            Status::WouldChange => self.would_change += 1,
            Status::WouldRun => self.would_run += 1,
            Status::WouldRunUnprobed => self.unprobed += 1,
        }
    }

    /// D15: `plan` exits 2 for anything it cannot call converged. Only `ok`
    /// and `skipped` are converged; `unknown` and `would run (unprobed)` are
    /// both provision admitting it does not know, which is not the same as
    /// nothing to do. `--plan-no-probe` is handled a level up: it inspected
    /// nothing, so it claims nothing.
    pub(crate) fn has_changes(&self) -> bool {
        self.total - (self.ok + self.skipped) > 0
    }

    /// The counts worth printing, in the order §9.1 shows them. A count of
    /// zero is noise, so it is left out.
    pub(crate) fn parts(&self) -> Vec<String> {
        let mut v = Vec::new();
        let mut add = |n: usize, word: &str| {
            if n > 0 {
                v.push(format!("{n} {word}"));
            }
        };
        add(self.changed, "changed");
        add(self.would_change, "would change");
        add(self.would_run, "would run");
        add(self.unprobed, "would run (unprobed)");
        add(self.ok, "ok");
        add(self.skipped, "skipped");
        add(self.unknown, "unknown");
        add(self.failed, "failed");
        v
    }
}

/// `1.7s`, `0.4s`, `12ms`. Durations in output are for a person scanning a
/// column, not for a benchmark.
pub(crate) fn human(d: Duration) -> String {
    let ms = d.as_millis();
    if ms < 1000 {
        format!("{ms}ms")
    } else {
        format!("{:.1}s", d.as_secs_f64())
    }
}

/// The last `n` lines, for the failure block. Spec §9.1 shows 20.
pub(crate) fn tail(text: &str, n: usize) -> Vec<&str> {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].to_vec()
}
