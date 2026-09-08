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

use crate::actions::{self, Action, Effect, Interpreter};
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
pub(crate) struct Prepared {
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
pub(crate) trait Judge {
    /// `failed_when`, or the default `result.rc != 0`.
    fn failed(&self, out: &Output) -> Result<bool>;
    /// `changed_when`, when the step declares one.
    fn changed(&self, out: &Output) -> Result<Option<bool>>;
    /// `assert: {expr: …}`.
    fn expr(&self, src: &str) -> Result<bool>;
}

pub(crate) struct Done {
    pub status: Status,
    /// What `register` binds. `None` when the step never ran.
    pub out: Option<Output>,
    pub attempt: u32,
    pub attempts: u32,
    /// A typed action's diff or metadata delta. Never set by `shell`.
    pub detail: Option<String>,
    /// Extra words for the status column, like `template`'s `3 of 14`.
    pub note: Option<String>,
}

impl Done {
    fn one(status: Status) -> Done {
        Done {
            status,
            out: None,
            attempt: 1,
            attempts: 1,
            detail: None,
            note: None,
        }
    }

    /// Ctrl-C rather than the step's own fault. `--keep-going` reads this:
    /// it carries on past a failure, never past an interrupt.
    pub(crate) fn interrupted(&self) -> bool {
        matches!(&self.status, Status::Failed(f) if f.interrupted)
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

pub(crate) struct Runner {
    pub sudo: Sudo,
    pub stream: bool,
    /// Whether `sudo` can actually escalate right now. `apply` proves this in
    /// its preflight and it is always true there. `plan` only asks — it is the
    /// read-only command and must still work on a machine with a cold sudo
    /// credential, so a root gate it cannot run is reported unprobed (D11).
    pub root_available: bool,
}

enum Gate {
    Run,
    Skip(String),
    /// A gate that needs root on a run that has none. Not an answer.
    NoRoot,
}

impl Runner {
    pub(crate) fn apply(&self, p: &Prepared, judge: &dyn Judge) -> Result<Done> {
        settle(self.apply_inner(p, judge))
    }

    /// Probe without mutating. `unless` runs — it is a question by contract —
    /// `creates` is a stat, and an `assert` is evaluated: spec §6.7 says plan
    /// runs asserts, because an assert failing at plan time is the cheapest
    /// way to learn the plan is aimed at the wrong machine.
    pub(crate) fn probe(&self, p: &Prepared, judge: &dyn Judge) -> Result<Done> {
        settle(self.probe_inner(p, judge))
    }

    fn apply_inner(&self, p: &Prepared, judge: &dyn Judge) -> R<Done> {
        match self.gate(p)? {
            Gate::Skip(why) => return Ok(Done::one(Status::Skipped(why))),
            Gate::NoRoot => return Err(Stop::Fail(no_root())),
            Gate::Run => {}
        }
        if p.action.is_typed() {
            return self.typed(p, judge, true);
        }
        let (attempts, delay) = match p.retry {
            Some(r) => (r.attempts, r.delay),
            None => (1, Duration::ZERO),
        };
        self.attempt_loop(p, judge, attempts, delay)
    }

    /// The retry loop. `attempts` is a parameter rather than read from `p`
    /// because `plan` runs asserts with exactly one attempt: a
    /// `retry: {attempts: 30, delay: 2s}` readiness gate is a sixty-second
    /// wait for work plan has not done and is not about to do.
    fn attempt_loop(
        &self,
        p: &Prepared,
        judge: &dyn Judge,
        attempts: u32,
        delay: Duration,
    ) -> R<Done> {
        for attempt in 1..=attempts {
            if process::interrupted() {
                return Err(Stop::Fail(interrupted()));
            }
            let out = self.once(p, judge)?;
            let fatal = out.how != How::Exited;
            let failed = fatal || judge.failed(&out)?;
            if !failed || fatal || attempt == attempts {
                return Self::verdict(p, judge, out, attempt, attempts);
            }
            if !delay.is_zero() {
                std::thread::sleep(delay);
            }
        }
        unreachable!("the loop returns on its last attempt")
    }

    fn probe_inner(&self, p: &Prepared, judge: &dyn Judge) -> R<Done> {
        match self.gate(p)? {
            Gate::Skip(why) => return Ok(Done::one(Status::Skipped(why))),
            // A gate that needs root when there is none is not a verdict.
            Gate::NoRoot => return Ok(Done::one(Status::WouldRunUnprobed)),
            Gate::Run => {}
        }
        if p.action.is_typed() {
            return self.typed(p, judge, false);
        }
        if let Action::Assert { .. } = &p.action {
            return self.attempt_loop(p, judge, 1, Duration::ZERO);
        }
        // A gate that said "run" is a verdict; no gate at all is not.
        Ok(Done::one(if p.gated() {
            Status::WouldRun
        } else {
            Status::Unknown
        }))
    }

    /// Run the action once. `assert: {expr: …}` spawns nothing, so it gets a
    /// synthetic result rather than its own path through the retry loop —
    /// `retry` has to behave identically for both assert forms.
    fn once(&self, p: &Prepared, judge: &dyn Judge) -> R<Output> {
        if let Action::Assert {
            expr: Some(src), ..
        } = &p.action
        {
            let ok = judge.expr(src)?;
            return Ok(Output {
                rc: i32::from(!ok),
                stdout: String::new(),
                stderr: String::new(),
                how: How::Exited,
            });
        }
        let argv = p
            .action
            .argv()
            .expect("every remaining action runs a command");
        self.spawn(p, argv, p.sudo)
    }

    fn spawn(&self, p: &Prepared, argv: Vec<String>, as_root: bool) -> R<Output> {
        let keys: Vec<String> = p.env.keys().cloned().collect();
        let argv = if as_root {
            self.sudo.wrap(argv, &keys)
        } else {
            argv
        };
        let stdin = if as_root { self.sudo.stdin() } else { None };
        process::run(&Spawn {
            argv: &argv,
            cwd: p.cwd.as_deref(),
            env: &p.env,
            stdin: stdin.as_deref(),
            timeout: p.timeout,
            stream: self.stream,
        })
        .map_err(|e| {
            let prog = argv.first().map_or("", String::as_str);
            Stop::Fail(Failure {
                msg: format!("cannot run `{prog}`: {e}"),
                rc: None,
                stderr: String::new(),
                interrupted: false,
            })
        })
    }

    /// Associated rather than a method: the verdict reads the step and the
    /// judge, and nothing about the runner. It stopped reading the runner
    /// when `ungated_ok` went.
    fn verdict(
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
                // Spec §6.1: nothing here can say what an ungated step did.
                // A step whose exit code is its whole contract declares
                // `changed_when: false` and is judged above; there is no verb
                // under which an undeclared step reads as anything but this.
                None => Status::Unknown,
            },
        };
        Ok(Done {
            status,
            out: Some(out),
            attempt,
            attempts,
            detail: None,
            note: None,
        })
    }

    /// A typed action inspects and changes state itself, so there is no exit
    /// code to judge. It is still given one — `0` or `1` — so that
    /// `changed_when`, `failed_when` and `register` mean the same thing on a
    /// `file` step as they do on a `shell` step.
    fn typed(&self, p: &Prepared, judge: &dyn Judge, act: bool) -> R<Done> {
        let ctx = actions::Ctx {
            sudo: p.sudo,
            root_available: self.root_available,
            escalate: &self.sudo,
            timeout: p.timeout,
            env: &p.env,
        };
        let (effect, mut note) = match &p.action {
            Action::File(spec) => (
                if act {
                    spec.apply(&ctx)
                } else {
                    spec.plan(&ctx)
                },
                None,
            ),
            Action::Service(spec) => (
                if act {
                    spec.apply(&ctx)
                } else {
                    spec.plan(&ctx)
                },
                None,
            ),
            Action::Pkg(spec) => (
                if act {
                    spec.apply(&ctx)
                } else {
                    spec.plan(&ctx)
                },
                None,
            ),
            Action::Template(spec) => {
                if act {
                    spec.apply(&ctx)
                } else {
                    spec.plan(&ctx)
                }
            }
            _ => unreachable!("is_typed and this match are the same set"),
        };

        let (mut status, detail) = match effect {
            Effect::Ok => (Status::Ok, None),
            Effect::Unknown => (Status::Unknown, None),
            Effect::Changed(d) => (
                if act {
                    Status::Changed
                } else {
                    Status::WouldChange
                },
                d,
            ),
            Effect::Unprobed => (Status::WouldRunUnprobed, None),
            Effect::Failed {
                msg,
                detail,
                interrupted,
            } => (
                Status::Failed(Failure {
                    msg,
                    rc: Some(1),
                    stderr: detail,
                    interrupted,
                }),
                None,
            ),
        };

        let out = Output {
            rc: i32::from(status.failed()),
            stdout: String::new(),
            stderr: String::new(),
            how: How::Exited,
        };
        // The overrides only speak where the action reached a verdict at all.
        if status.failed() || matches!(status, Status::WouldRunUnprobed) {
            note = None;
        }
        if !status.failed() && !matches!(status, Status::WouldRunUnprobed) {
            if judge.failed(&out)? {
                status = Status::Failed(Failure {
                    msg: "failed_when".into(),
                    rc: Some(0),
                    stderr: String::new(),
                    interrupted: false,
                });
            } else if let Some(changed) = judge.changed(&out)? {
                status = match (changed, act) {
                    (true, true) => Status::Changed,
                    (true, false) => Status::WouldChange,
                    (false, _) => Status::Ok,
                };
            }
        }

        Ok(Done {
            status,
            out: Some(out),
            attempt: 1,
            attempts: 1,
            detail,
            note,
        })
    }

    fn gate(&self, p: &Prepared) -> R<Gate> {
        if let Some(cmd) = &p.unless {
            // Spec §4: the gate runs with the step's sudo, env and cwd. A root
            // step's `unless: test -f /root/.x` has to see root's view, or the
            // gate answers a question nobody asked.
            if p.sudo && !self.root_available {
                return Ok(Gate::NoRoot);
            }
            let argv = Interpreter::default_for_host().argv(cmd, false);
            let out = self.spawn(&gate_context(p), argv, p.sudo)?;
            match out.how {
                How::Exited if out.rc == 0 => return Ok(Gate::Skip("unless".into())),
                // Spec §10: an `unless` that cannot run is an error, not a
                // licence to run the step. 127 is the interpreter saying it
                // could not find the command and 126 that it could not execute
                // it; either way "the check is broken" and "the work is not
                // done" are not the same answer, and only one of them is safe
                // to assume.
                How::Exited if out.rc == 126 || out.rc == 127 => {
                    return Err(Stop::Fail(Failure {
                        msg: format!(
                            "`unless` could not run: {}",
                            cmd.lines().next().unwrap_or("")
                        ),
                        rc: Some(out.rc),
                        stderr: out.stderr,
                        interrupted: false,
                    }));
                }
                // Same rule one line up, for the other way a gate fails to
                // give an answer. A hung `unless` told us nothing, and
                // "the check never finished" is not "the work is not done".
                // Provisional (2026-09-08 review, owner to confirm): it turns
                // a silent re-run into a failure.
                How::TimedOut => {
                    return Err(Stop::Fail(Failure {
                        msg: format!(
                            "`unless` timed out after {}: {}",
                            crate::output::event::human(p.timeout),
                            cmd.lines().next().unwrap_or("")
                        ),
                        rc: None,
                        stderr: out.stderr,
                        interrupted: false,
                    }));
                }
                How::Interrupted => return Err(Stop::Fail(interrupted())),
                How::Exited => {}
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

/// A gate command runs with the step's `sudo`, `env` and `cwd` (spec §4), and
/// nothing else of the step's: never its retry, never its own gates.
fn gate_context(p: &Prepared) -> Prepared {
    Prepared {
        action: Action::Gate,
        unless: None,
        creates: None,
        cwd: p.cwd.clone(),
        env: p.env.clone(),
        sudo: p.sudo,
        timeout: p.timeout,
        retry: None,
        has_changed_when: false,
    }
}

fn no_root() -> Failure {
    Failure {
        msg: "this step's `unless` needs root and sudo is not available".into(),
        rc: None,
        stderr: String::new(),
        interrupted: false,
    }
}

fn interrupted() -> Failure {
    Failure {
        msg: "interrupted".into(),
        rc: None,
        stderr: String::new(),
        interrupted: true,
    }
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
