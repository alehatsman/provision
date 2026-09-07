//! The rendered actions. Phase 1 ships the three that reduce to "run a
//! command and judge it by its exit code": `shell`, `cmd`, `assert`.
//!
//! They share one implementation because they genuinely are one — the
//! differences are which argv gets built and what a success means. Phase 2's
//! actions (`file`, `template`, `pkg`, `service`) each need real state
//! inspection and get their own modules then.

pub mod file;
pub mod service;
pub mod template;

use crate::config::model::{self, Step};
use crate::error::Result;
use crate::exec::process;
use crate::exec::sudo::Sudo;
use crate::template::Engine;
use crate::yaml::N;
use minijinja::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Shell { script: String, interpreter: Interpreter, login: bool },
    Cmd(Vec<String>),
    Assert { command: Option<String>, expr: Option<String>, msg: Option<String> },
    File(file::Spec),
    Template(template::Spec),
    Service(service::Spec),
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
        matches!(self, Action::File(_) | Action::Template(_) | Action::Service(_))
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
    /// Differs. The string is the diff or the metadata delta, if there is one
    /// worth printing.
    Changed(Option<String>),
    Failed { msg: String, detail: String },
    /// Could not look: the answer needs a root this run does not have. Plan
    /// only — `apply` proved sudo works before the first step.
    Unprobed,
}

/// The part of the run a typed action needs. Two questions and a way to ask
/// them as root; nothing about scopes, templates, or the walk.
pub struct Ctx<'a> {
    /// The step's own `sudo: true`.
    pub sudo: bool,
    /// Whether escalation works at all. Only `plan` ever sees this false.
    pub root_available: bool,
    pub escalate: &'a Sudo,
}

impl Ctx<'_> {
    /// Run a command as root and capture its bytes exactly.
    pub fn as_root(&self, argv: &[&str]) -> std::io::Result<process::Captured> {
        self.exec(argv, true)
    }

    /// Run a command, escalating only when asked. Probes usually should not:
    /// `systemctl is-active` answers for anyone, and asking for root to read
    /// a state that is world-readable is how `plan` stops working on a
    /// machine with a cold credential.
    pub fn exec(&self, argv: &[&str], as_root: bool) -> std::io::Result<process::Captured> {
        let argv: Vec<String> = argv.iter().map(|s| (*s).to_string()).collect();
        let (argv, stdin) = if as_root {
            (self.escalate.wrap(argv, &[]), self.escalate.stdin())
        } else {
            (argv, None)
        };
        process::capture(&argv, stdin.as_deref())
    }

    /// Spec §6.3: with `sudo: true` every probe of the target runs as root,
    /// because a target the step needs root to write is usually one it needs
    /// root to read.
    pub fn reads_as_root(&self) -> bool {
        self.sudo
    }
}
