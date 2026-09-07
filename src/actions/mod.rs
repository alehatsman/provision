//! The rendered actions. Phase 1 ships the three that reduce to "run a
//! command and judge it by its exit code": `shell`, `cmd`, `assert`.
//!
//! They share one implementation because they genuinely are one — the
//! differences are which argv gets built and what a success means. Phase 2's
//! actions (`file`, `template`, `pkg`, `service`) each need real state
//! inspection and get their own modules then.

pub mod file;
pub mod pkg;
pub mod service;
pub mod template;

use crate::config::model::{self, Step};
use crate::error::Result;
use crate::exec::process;
use crate::exec::sudo::Sudo;
use crate::template::Engine;
use crate::yaml::N;
use minijinja::Value;
use std::collections::BTreeMap;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Shell { script: String, interpreter: Interpreter, login: bool },
    Cmd(Vec<String>),
    Assert { command: Option<String>, expr: Option<String>, msg: Option<String> },
    File(file::Spec),
    Template(template::Spec),
    Service(service::Spec),
    Pkg(pkg::Spec),
    /// Parsed and validated, but with no runner until phase 2. `plan` reports
    /// it unprobed; `apply` refuses rather than pretending it converged.
    NotYet(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interpreter {
    Bash,
    Sh,
    Zsh,
    PowerShell,
    Pwsh,
}

impl Interpreter {
    /// Spec §6.1: bash on unix, powershell on Windows.
    pub fn default_for_host() -> Interpreter {
        if cfg!(windows) { Interpreter::PowerShell } else { Interpreter::Bash }
    }

    pub fn parse(name: &str, at: N<'_>) -> Result<Interpreter> {
        Ok(match name {
            "bash" => Interpreter::Bash,
            "sh" => Interpreter::Sh,
            "zsh" => Interpreter::Zsh,
            "powershell" => Interpreter::PowerShell,
            "pwsh" => Interpreter::Pwsh,
            other => {
                return Err(at
                    .err(format!("unknown interpreter `{other}`"))
                    .with_note("one of: bash, sh, zsh, powershell, pwsh"));
            }
        })
    }

    pub fn argv(self, script: &str, login: bool) -> Vec<String> {
        let s = |v: &str| v.to_string();
        match self {
            Interpreter::Bash | Interpreter::Sh | Interpreter::Zsh => {
                let prog = match self {
                    Interpreter::Bash => "bash",
                    Interpreter::Sh => "sh",
                    _ => "zsh",
                };
                let mut v = vec![s(prog)];
                if login {
                    v.push(s("-l"));
                }
                v.push(s("-c"));
                v.push(script.to_string());
                v
            }
            Interpreter::PowerShell | Interpreter::Pwsh => {
                let prog = if self == Interpreter::Pwsh { "pwsh" } else { "powershell" };
                // -NonInteractive so a step that would prompt fails instead of
                // hanging behind a spinner with no way to type at it.
                vec![
                    s(prog),
                    s("-NoProfile"),
                    s("-NonInteractive"),
                    s("-Command"),
                    script.to_string(),
                ]
            }
        }
    }
}

impl Action {
    /// The command to run, if this action runs one. `assert` with an `expr`
    /// and every phase 2 action return `None`.
    pub fn argv(&self) -> Option<Vec<String>> {
        match self {
            Action::Shell { script, interpreter, login } => {
                Some(interpreter.argv(script, *login))
            }
            Action::Cmd(argv) => Some(argv.clone()),
            Action::Assert { command: Some(c), .. } => {
                Some(Interpreter::default_for_host().argv(c, false))
            }
            _ => None,
        }
    }

    /// Spec §6.7: an assert never changes anything, so its success is `ok`
    /// and never `changed`. `shell` and `cmd` are judged by their gate.
    pub fn never_changes(&self) -> bool {
        matches!(self, Action::Assert { .. })
    }

    /// An action that inspects and changes state itself, rather than reporting
    /// through an exit code. It never reaches the runner's argv path.
    pub fn is_typed(&self) -> bool {
        matches!(self, Action::File(_) | Action::Template(_) | Action::Service(_) | Action::Pkg(_))
    }
}

impl Action {
    /// Parse and render one step's action body.
    ///
    /// `Ok(None)` means a field would not render. The expander's own sweep has
    /// already reported that, with a position — reporting it again here would
    /// turn one bad `{{ … }}` into two diagnostics.
    pub fn parse(
        engine: &Engine,
        step: &Step<'_>,
        ctx: &Value,
        raw: bool,
    ) -> Result<Option<Action>> {
        let body = step.body;
        // `raw: true` means the body is not a template (spec §4). It still
        // reaches the action; it just arrives as written.
        let render = |src: &str| -> Option<String> {
            if raw { Some(src.to_string()) } else { engine.render(src, ctx).ok() }
        };

        let action = match step.key {
            "shell" => {
                let long = body.as_str().is_err();
                let script_at = if long {
                    match body.get("script") {
                        Some(n) => n,
                        None => {
                            return Err(body
                                .err("`shell` requires `script`")
                                .with_note("or write the script directly: `shell: |`"));
                        }
                    }
                } else {
                    body
                };
                let Some(script) = render(script_at.as_str()?) else { return Ok(None) };
                let interpreter = match long.then(|| body.get("interpreter")).flatten() {
                    Some(n) => {
                        let Some(name) = render(n.as_str()?) else { return Ok(None) };
                        Interpreter::parse(&name, n)?
                    }
                    None => Interpreter::default_for_host(),
                };
                let login = body.get("login").and_then(|n| n.as_bool().ok()).unwrap_or(false);
                Action::Shell { script, interpreter, login }
            }

            "cmd" => {
                // A single expression may hold the whole argv, which is how a
                // list built in `vars` reaches a `cmd` (spec §3.4).
                let items: Vec<Value> = match body.as_seq() {
                    Ok(nodes) => {
                        let mut v = Vec::with_capacity(nodes.len());
                        for n in nodes {
                            match engine.render_node(n, ctx) {
                                Ok(x) => v.push(x),
                                Err(_) => return Ok(None),
                            }
                        }
                        v
                    }
                    Err(_) => {
                        let Ok(value) = engine.render_node(body, ctx) else { return Ok(None) };
                        match value.try_iter() {
                            Ok(it) => it.collect(),
                            Err(_) => {
                                return Err(body
                                    .err("`cmd` is a list of arguments")
                                    .with_note("cmd: [mv, src, dest] — use `shell` for a script"));
                            }
                        }
                    }
                };
                if items.is_empty() {
                    return Err(body.err("`cmd` is empty"));
                }
                Action::Cmd(items.iter().map(|v| v.to_string()).collect())
            }

            "assert" => {
                let field = |k: &str| -> Option<String> {
                    body.get(k).and_then(|n| n.as_str().ok()).and_then(&render)
                };
                let command = field("command");
                // An `expr` is an expression, not a template: it is evaluated
                // against the scope at run time, not rendered into text first.
                let expr = body.get("expr").and_then(|n| n.as_str().ok()).map(str::to_string);
                if command.is_none() && expr.is_none() {
                    return Err(body.err("`assert` needs `command` or `expr`").with_note(
                        "command: a shell command that exits 0 · expr: an expression",
                    ));
                }
                Action::Assert { command, expr, msg: field("msg") }
            }

            "file" => match file::parse(step, engine, ctx, raw)? {
                Some(spec) => Action::File(spec),
                None => return Ok(None),
            },

            "pkg" => match pkg::parse(step, engine, ctx, raw)? {
                Some(spec) => Action::Pkg(spec),
                None => return Ok(None),
            },

            "service" => match service::parse(step, engine, ctx, raw)? {
                Some(spec) => Action::Service(spec),
                None => return Ok(None),
            },

            "template" => match template::parse(step, engine, ctx, raw)? {
                Some(spec) => Action::Template(spec),
                None => return Ok(None),
            },

            other => Action::NotYet(static_key(other)),
        };
        Ok(Some(action))
    }
}

/// The action key, for an action whose runner arrives in phase 2. Every key is
/// one of a fixed set, so this borrows nothing that was not already static.
fn static_key(key: &str) -> &'static str {
    model::ACTION_KEYS
        .iter()
        .chain(model::STRUCTURAL_KEYS)
        .find(|k| **k == key)
        .copied()
        .unwrap_or("action")
}

// ── typed actions ─────────────────────────────────────────────────────────

/// What a typed action did, or would do.
///
/// Phase 1's three actions report through an exit code, which is why they run
/// through the runner's argv path. These four do their own work and have to
/// say what they did in their own words.
pub enum Effect {
    /// Already as declared.
    Ok,
    /// Cannot tell without asking a remote. `pkg` with `state: latest` on a
    /// package that is installed: whether a newer version exists is not a
    /// question the local database answers.
    Unknown,
    /// Differs. The string is the diff or the metadata delta, if there is one
    /// worth printing.
    Changed(Option<String>),
    Failed {
        msg: String,
        detail: String,
        /// Ctrl-C, not the step's own fault. Drives the exit code, not the
        /// glyph.
        interrupted: bool,
    },
    /// Could not look: the answer needs a root this run does not have. Plan
    /// only — `apply` proved sudo works before the first step.
    Unprobed,
}

/// The part of the run a typed action needs. Two questions and a way to ask
/// them; nothing about scopes, templates, or the walk.
pub struct Ctx<'a> {
    /// The step's own `sudo: true`.
    pub sudo: bool,
    /// Whether escalation works at all. Only `plan` ever sees this false.
    pub root_available: bool,
    pub escalate: &'a Sudo,
    /// The step's `timeout` (spec §4). Every command a typed action runs is
    /// bound by it — `systemctl start` waits on the unit's own timeout and
    /// `apt-get install` waits forever on a dpkg lock, and §4 promises the
    /// step is killed either way.
    pub timeout: Duration,
    pub env: &'a BTreeMap<String, String>,
}

impl Ctx<'_> {
    pub fn as_root(&self, argv: &[&str]) -> std::io::Result<process::Raw> {
        self.exec(argv, true)
    }

    /// Run a command, escalating only when asked.
    ///
    /// Probes usually should not: `systemctl is-active` and `dpkg-query`
    /// answer for anyone, and asking for root to read a world-readable state
    /// is how `plan` stops working on a machine with a cold credential.
    ///
    /// Probes and mutations take the same path regardless, because a hung
    /// manager hangs `is-active` exactly as it hangs `start`.
    pub fn exec(&self, argv: &[&str], as_root: bool) -> std::io::Result<process::Raw> {
        let argv: Vec<String> = argv.iter().map(|s| (*s).to_string()).collect();
        let (argv, stdin) = if as_root {
            (self.escalate.wrap(argv, &[]), self.escalate.stdin())
        } else {
            (argv, None)
        };
        process::capture(process::Spawn {
            argv: &argv,
            cwd: None,
            env: self.env,
            stdin: stdin.as_deref(),
            timeout: self.timeout,
            stream: false,
        })
    }

    /// Run a command that does work. `None` means it succeeded.
    ///
    /// The message is the **first** non-empty stderr line, not the last:
    /// systemd ends with "See `systemctl status ...`", which is a pointer,
    /// not a reason. The whole stderr goes to the detail, so the failure
    /// block shows what happened.
    pub fn perform(&self, argv: &[&str], as_root: bool) -> Option<Effect> {
        let got = match self.exec(argv, as_root) {
            Ok(g) => g,
            Err(e) => return Some(Effect::fail(format!("cannot run {}: {e}", argv[0]))),
        };
        if let Some(bad) = self.stopped(&got) {
            return Some(bad);
        }
        if got.rc != 0 {
            let stderr = got.stderr_text();
            let why = stderr
                .lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .unwrap_or("failed")
                .to_string();
            return Some(Effect::Failed {
                msg: format!("{} failed: {why}", argv[0]),
                detail: stderr.trim().to_string(),
                interrupted: false,
            });
        }
        None
    }

    /// The wording the shell path uses for a command that did not get to
    /// finish, so a `file` step and a `shell` step read the same when the
    /// same thing happened to them.
    pub fn stopped(&self, got: &process::Raw) -> Option<Effect> {
        match got.how {
            process::How::Exited => None,
            process::How::TimedOut => Some(Effect::Failed {
                msg: format!("timed out after {}", crate::output::event::human(self.timeout)),
                detail: got.stderr_text(),
                interrupted: false,
            }),
            process::How::Interrupted => Some(Effect::Failed {
                msg: "interrupted".into(),
                detail: String::new(),
                interrupted: true,
            }),
        }
    }

    /// Spec §6.3: with `sudo: true` every probe of the target runs as root,
    /// because a target the step needs root to write is usually one it needs
    /// root to read.
    pub fn reads_as_root(&self) -> bool {
        self.sudo
    }
}

impl Effect {
    /// A failure with no captured output behind it.
    pub fn fail(msg: impl Into<String>) -> Effect {
        Effect::Failed { msg: msg.into(), detail: String::new(), interrupted: false }
    }
}
