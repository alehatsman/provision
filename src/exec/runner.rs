//! The step pipeline: gates, retry, timeout, and the verdict.
//!
//! Spec §4 fixes the order — `when` → `unless` → `creates` → action →
//! `failed_when` → `changed_when` → `register`. `when` belongs to expansion,
//! because it decides whether the step exists at all; everything from
//! `unless` onward lives here.
//!
//! Expression evaluation is not here. The runner asks a `Judge` — implemented
//! by the expander, which owns the template engine and the scope — so that
//! `failed_when` can be consulted *inside* the retry loop. That is what makes
//! `retry` on an `assert` a readiness gate (spec §6.7) rather than a rerun of
//! something already deemed fine.

use crate::actions::{Action, Interpreter};
use crate::config::model::Retry;
use crate::error::{Diag, Result};
use crate::exec::process::{self, How, Output, Spawn};
use crate::exec::sudo::Sudo;
use crate::output::event::{Failure, Status};
use crate::template::expanduser;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

/// One step, rendered, ready to run.
pub struct Prepared {
    pub action: Action,
    pub unless: Option<String>,
    pub creates: Option<String>,
    pub cwd: Option<PathBuf>,
    pub env: BTreeMap<String, String>,
    pub sudo: bool,
    pub timeout: Duration,
    pub retry: Option<Retry>,
    pub has_changed_when: bool,
}

impl Prepared {
    /// Spec §6.1: a step is judged `changed` only when something declared what
    /// changing means. Without a gate the honest answer is `unknown`.
    fn gated(&self) -> bool {
        self.unless.is_some() || self.creates.is_some() || self.has_changed_when
    }
}

/// What the runner needs from the expander's scope. Three questions, so the
/// runner never has to know about scopes or templates.
pub trait Judge {
    /// `failed_when`, or the default `result.rc != 0`.
    fn failed(&self, out: &Output) -> Result<bool>;
    /// `changed_when`, when the step declares one.
    fn changed(&self, out: &Output) -> Result<Option<bool>>;
    /// `assert: {expr: …}`.
    fn expr(&self, src: &str) -> Result<bool>;
}

pub struct Done {
    pub status: Status,
    /// What `register` binds. `None` when the step never ran.
    pub out: Option<Output>,
    pub attempt: u32,
    pub attempts: u32,
}

impl Done {
    fn one(status: Status) -> Done {
        Done { status, out: None, attempt: 1, attempts: 1 }
    }
}

/// Two ways a step can stop early. A `Diag` is provision's own fault — a bad
/// expression, a broken plan — and aborts with a position. A `Failure` is the
/// step's outcome, and belongs on the step's own line.
enum Stop {
    Bad(Diag),
    Fail(Failure),
}

impl From<Diag> for Stop {
    fn from(d: Diag) -> Stop {
        Stop::Bad(d)
    }
}

type R<T> = std::result::Result<T, Stop>;

pub struct Runner {
    pub sudo: Sudo,
    pub stream: bool,
}

enum Gate {
    Run,
    Skip(String),
}

impl Runner {
    pub fn apply(&self, p: &Prepared, judge: &dyn Judge) -> Result<Done> {
        settle(self.apply_inner(p, judge))
    }

    /// Probe without mutating. `unless` runs — it is a question by contract —
    /// `creates` is a stat, and an `assert` is evaluated: spec §6.7 says plan
    /// runs asserts, because an assert failing at plan time is the cheapest
    /// way to learn the plan is aimed at the wrong machine.
    pub fn probe(&self, p: &Prepared, judge: &dyn Judge) -> Result<Done> {
        settle(self.probe_inner(p, judge))
    }

    fn apply_inner(&self, p: &Prepared, judge: &dyn Judge) -> R<Done> {
        if let Action::NotYet(key) = p.action {
            // Reached only because validation lets it through, which it must:
            // `plan --plan-no-probe` has to keep working on the whole tree.
            return Err(Stop::Fail(Failure {
                msg: format!("`{key}` is not implemented yet (phase 2)"),
                rc: None,
                stderr: String::new(),
                interrupted: false,
            }));
        }

        match self.gate(p)? {
            Gate::Skip(why) => return Ok(Done::one(Status::Skipped(why))),
            Gate::Run => {}
        }

        let (attempts, delay) = match p.retry {
            Some(r) => (r.attempts, r.delay),
            None => (1, Duration::ZERO),
        };

        for attempt in 1..=attempts {
            if process::interrupted() {
                return Err(Stop::Fail(interrupted()));
            }
            let out = self.once(p, judge)?;
            let fatal = out.how != How::Exited;
            let failed = fatal || judge.failed(&out)?;
            if !failed || fatal || attempt == attempts {
                return self.verdict(p, judge, out, attempt, attempts);
            }
            if !delay.is_zero() {
                std::thread::sleep(delay);
            }
        }
        unreachable!("the loop returns on its last attempt")
    }

    fn probe_inner(&self, p: &Prepared, judge: &dyn Judge) -> R<Done> {
        if let Action::NotYet(_) = p.action {
            return Ok(Done::one(Status::WouldRunUnprobed));
        }
        match self.gate(p)? {
            Gate::Skip(why) => return Ok(Done::one(Status::Skipped(why))),
            Gate::Run => {}
        }
        if let Action::Assert { .. } = &p.action {
            return self.apply_inner(p, judge);
        }
        // A gate that said "run" is a verdict; no gate at all is not.
        Ok(Done::one(if p.gated() { Status::WouldRun } else { Status::Unknown }))
    }

    /// Run the action once. `assert: {expr: …}` spawns nothing, so it gets a
    /// synthetic result rather than its own path through the retry loop —
    /// `retry` has to behave identically for both assert forms.
    fn once(&self, p: &Prepared, judge: &dyn Judge) -> R<Output> {
        if let Action::Assert { expr: Some(src), .. } = &p.action {
            let ok = judge.expr(src)?;
            return Ok(Output {
                rc: i32::from(!ok),
                stdout: String::new(),
                stderr: String::new(),
                how: How::Exited,
            });
        }
        let argv = p.action.argv().expect("every remaining action runs a command");
        self.spawn(p, argv, p.sudo)
    }

    fn spawn(&self, p: &Prepared, argv: Vec<String>, as_root: bool) -> R<Output> {
        let keys: Vec<String> = p.env.keys().cloned().collect();
        let argv = if as_root { self.sudo.wrap(argv, &keys) } else { argv };
        let stdin = if as_root { self.sudo.stdin() } else { None };
        process::run(Spawn {
            argv: &argv,
            cwd: p.cwd.as_deref(),
            env: &p.env,
            stdin: stdin.as_deref(),
            timeout: p.timeout,
            stream: self.stream,
        })
        .map_err(|e| {
            let prog = argv.first().map(String::as_str).unwrap_or("");
            Stop::Fail(Failure {
                msg: format!("cannot run `{prog}`: {e}"),
                rc: None,
                stderr: String::new(),
                interrupted: false,
            })
        })
    }

    fn verdict(
        &self,
        p: &Prepared,
        judge: &dyn Judge,
        out: Output,
        attempt: u32,
        attempts: u32,
    ) -> R<Done> {
        let status = match out.how {
            How::Interrupted => Status::Failed(interrupted()),
            How::TimedOut => Status::Failed(Failure {
                msg: format!("timed out after {}", crate::output::event::human(p.timeout)),
                rc: None,
                stderr: out.stderr.clone(),
                interrupted: false,
            }),
            How::Exited if judge.failed(&out)? => Status::Failed(Failure {
                // Spec §6.7: an assert's `msg` is what the operator reads, so
                // it replaces the generic wording rather than joining it.
                msg: match &p.action {
                    Action::Assert { msg: Some(m), .. } => m.clone(),
                    _ => "exit".into(),
                },
                rc: Some(out.rc),
                stderr: out.stderr.clone(),
                interrupted: false,
            }),
            How::Exited => match judge.changed(&out)? {
                Some(true) => Status::Changed,
                Some(false) => Status::Ok,
                None if p.action.never_changes() => Status::Ok,
                // The gate said the work was needed, and the work succeeded.
                None if p.gated() => Status::Changed,
                None => Status::Unknown,
            },
        };
        Ok(Done { status, out: Some(out), attempt, attempts })
    }

    fn gate(&self, p: &Prepared) -> R<Gate> {
        if let Some(cmd) = &p.unless {
            let argv = Interpreter::default_for_host().argv(cmd, false);
            // Spec §10: an `unless` that cannot be spawned at all is an error,
            // not a licence to run the step. "The check is broken" and "the
            // work is not done" are not the same answer.
            let out = self.spawn(&gate_context(p), argv, false)?;
            match out.how {
                How::Exited if out.rc == 0 => return Ok(Gate::Skip("unless".into())),
                How::Interrupted => return Err(Stop::Fail(interrupted())),
                _ => {}
            }
        }
        if let Some(path) = &p.creates
            && std::path::Path::new(&expanduser(path)).exists()
        {
            return Ok(Gate::Skip("creates exists".into()));
        }
        Ok(Gate::Run)
    }
}

/// A gate command runs with the step's `env` and `cwd` (spec §4) and nothing
/// else of the step's: never its sudo, never its retry.
fn gate_context(p: &Prepared) -> Prepared {
    Prepared {
        action: Action::NotYet("unless"),
        unless: None,
        creates: None,
        cwd: p.cwd.clone(),
        env: p.env.clone(),
        sudo: false,
        timeout: p.timeout,
        retry: None,
        has_changed_when: false,
    }
}

fn interrupted() -> Failure {
    Failure { msg: "interrupted".into(), rc: None, stderr: String::new(), interrupted: true }
}

/// A `Failure` is an outcome, not an error: it lands on the step's line and
/// the walk decides what to do about it.
fn settle(r: R<Done>) -> Result<Done> {
    match r {
        Ok(d) => Ok(d),
        Err(Stop::Fail(f)) => Ok(Done::one(Status::Failed(f))),
        Err(Stop::Bad(d)) => Err(d),
    }
}
