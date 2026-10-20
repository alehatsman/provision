//! The rendered actions. Three reduce to "run a command and judge it by its
//! exit code": `shell`, `cmd`, `assert`.
//!
//! Those share one implementation because they genuinely are one — the
//! differences are which argv gets built and what a success means. The typed
//! actions (`file`, `template`, `pkg`, `service`, `git`, `defaults`,
//! `download`, `json`) each need real state inspection and have their own
//! modules.

pub(crate) mod defaults;
pub(crate) mod download;
pub(crate) mod file;
pub(crate) mod git;
pub(crate) mod json;
pub(crate) mod pkg;
pub(crate) mod service;
pub(crate) mod template;

use crate::config::model::Step;
use crate::error::Result;
use crate::exec::process;
use crate::exec::sudo::Sudo;
use crate::template::Engine;
use crate::yaml::N;
use minijinja::Value;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Action {
    Shell {
        script: String,
        interpreter: Interpreter,
        login: bool,
    },
    Cmd(Vec<String>),
    Assert {
        command: Option<String>,
        expr: Option<String>,
        msg: Option<String>,
    },
    Defaults(defaults::Spec),
    Download(download::Spec),
    File(file::Spec),
    Git(git::Spec),
    Json(json::Spec),
    Template(template::Spec),
    Service(service::Spec),
    Pkg(pkg::Spec),
    /// The stand-in a gate's spawn context carries. `unless` runs an argv
    /// the runner builds itself, so `gate_context` needs an `Action` it will
    /// never look at — and naming that honestly beats reusing a real variant.
    Gate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Interpreter {
    Bash,
    Sh,
    Zsh,
    PowerShell,
    Pwsh,
}

impl Interpreter {
    /// Spec §6.1: bash on unix, powershell on Windows.
    pub(crate) fn default_for_host() -> Interpreter {
        if cfg!(windows) {
            Interpreter::PowerShell
        } else {
            Interpreter::Bash
        }
    }

    pub(crate) fn parse(name: &str, at: N<'_>) -> Result<Interpreter> {
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

    pub(crate) fn argv(self, script: &str, login: bool) -> Vec<String> {
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
                let prog = if self == Interpreter::Pwsh {
                    "pwsh"
                } else {
                    "powershell"
                };
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
    /// and every typed action return `None`.
    pub(crate) fn argv(&self) -> Option<Vec<String>> {
        match self {
            Action::Shell {
                script,
                interpreter,
                login,
            } => Some(interpreter.argv(script, *login)),
            Action::Cmd(argv) => Some(argv.clone()),
            Action::Assert {
                command: Some(c), ..
            } => Some(Interpreter::default_for_host().argv(c, false)),
            _ => None,
        }
    }

    /// Spec §6.7: an assert never changes anything, so its success is `ok`
    /// and never `changed`. `shell` and `cmd` are judged by their gate.
    pub(crate) fn never_changes(&self) -> bool {
        matches!(self, Action::Assert { .. })
    }

    /// The step key this variant was parsed from, or `None` for the gate's
    /// stand-in. No wildcard: a new variant fails the build until it says
    /// which table row it belongs to.
    pub(crate) fn key(&self) -> Option<&'static str> {
        match self {
            Action::Shell { .. } => Some("shell"),
            Action::Cmd(_) => Some("cmd"),
            Action::Assert { .. } => Some("assert"),
            Action::Defaults(_) => Some("defaults"),
            Action::Download(_) => Some("download"),
            Action::File(_) => Some("file"),
            Action::Git(_) => Some("git"),
            Action::Json(_) => Some("json"),
            Action::Template(_) => Some("template"),
            Action::Service(_) => Some("service"),
            Action::Pkg(_) => Some("pkg"),
            Action::Gate => None,
        }
    }

    /// An action that inspects and changes state itself, rather than reporting
    /// through an exit code. It never reaches the runner's argv path. Read from
    /// the action table, so the answer is stated once, beside the docs.
    pub(crate) fn is_typed(&self) -> bool {
        self.key()
            .and_then(crate::config::model::action_doc)
            .is_some_and(|d| d.typed)
    }
}

impl Action {
    /// Parse and render one step's action body.
    ///
    /// `Ok(None)` means a field would not render. The expander's own sweep has
    /// already reported that, with a position — reporting it again here would
    /// turn one bad `{{ … }}` into two diagnostics.
    ///
    /// `defer_src` says a `file` step's `src` may not exist yet: an earlier
    /// `shell` or `cmd` step in the same plan can create any path, and on a
    /// walk that runs nothing it has not created it. Such a step parses to
    /// `Ok(None)` — unresolved rather than broken — and the walk that executes
    /// passes `false`, so a source that is still missing when the step runs is
    /// the error it always was (spec §6.3, D18).
    pub(crate) fn parse(
        engine: &Engine,
        step: &Step<'_>,
        ctx: &Value,
        raw: bool,
        defer_src: bool,
    ) -> Result<Option<Action>> {
        let body = step.body;
        let render = |src: &str| rendered(engine, ctx, raw, src);

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
                let Some(script) = render(script_at.as_str()?) else {
                    return Ok(None);
                };
                let interpreter = match long.then(|| body.get("interpreter")).flatten() {
                    Some(n) => {
                        let Some(name) = render(n.as_str()?) else {
                            return Ok(None);
                        };
                        Interpreter::parse(&name, n)?
                    }
                    None => Interpreter::default_for_host(),
                };
                // A YAML boolean or an error, not a silent `false` (see `pkg`'s
                // `cask`): `login: "true"` ran a non-login shell.
                let login = match body.get("login") {
                    Some(n) => n.as_bool()?,
                    None => false,
                };
                Action::Shell {
                    script,
                    interpreter,
                    login,
                }
            }

            "cmd" => {
                // A single expression may hold the whole argv, which is how a
                // list built in `vars` reaches a `cmd` (spec §3.4).
                #[expect(
                    clippy::single_match_else,
                    reason = "both arms build the same Vec by different routes; an if-let hides that"
                )]
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
                        let Ok(value) = engine.render_node(body, ctx) else {
                            return Ok(None);
                        };
                        // A string answers `try_iter` with its characters, so
                        // `cmd: "echo hi"` became the argv `e c h o …` and
                        // failed at run time as `cannot run e`. `pkg`'s
                        // `names` guards the same trap the same way.
                        if value.kind() != minijinja::value::ValueKind::Seq {
                            return Err(body
                                .err("`cmd` is a list of arguments")
                                .with_note("cmd: [mv, src, dest] — use `shell` for a script"));
                        }
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
                Action::Cmd(items.iter().map(ToString::to_string).collect())
            }

            "assert" => {
                let field = |k: &str| -> Option<String> {
                    body.get(k).and_then(|n| n.as_str().ok()).and_then(render)
                };
                // The second of the two in document order, whichever it is.
                let mut forms = body
                    .keys()?
                    .into_iter()
                    .filter(|(k, _)| matches!(*k, "command" | "expr"));
                if let (Some(_), Some((_, second))) = (forms.next(), forms.next()) {
                    return Err(second
                        .err("`command` and `expr` are mutually exclusive")
                        .with_note(
                            "command runs a shell command; expr is evaluated, nothing runs",
                        ));
                }
                let command = field("command");
                // An `expr` is an expression, not a template: it is evaluated
                // against the scope at run time, not rendered into text first.
                let expr = body
                    .get("expr")
                    .and_then(|n| n.as_str().ok())
                    .map(str::to_string);
                if command.is_none() && expr.is_none() {
                    return Err(body
                        .err("`assert` needs `command` or `expr`")
                        .with_note("command: a shell command that exits 0 · expr: an expression"));
                }
                Action::Assert {
                    command,
                    expr,
                    msg: field("msg"),
                }
            }

            "file" => match file::parse(step, engine, ctx, raw, defer_src)? {
                Some(spec) => Action::File(spec),
                None => return Ok(None),
            },

            "defaults" => match defaults::parse(step, engine, ctx, raw)? {
                Some(spec) => Action::Defaults(spec),
                None => return Ok(None),
            },

            "download" => match download::parse(step, engine, ctx, raw)? {
                Some(spec) => Action::Download(spec),
                None => return Ok(None),
            },

            "json" => match json::parse(step, engine, ctx, raw, defer_src)? {
                Some(spec) => Action::Json(spec),
                None => return Ok(None),
            },

            "git" => match git::parse(step, engine, ctx, raw)? {
                Some(spec) => Action::Git(spec),
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

            // Unreachable in practice: `model` rejects any key that is not
            // an action, a structural keyword or a modifier before parsing
            // gets here, and every action key above is handled. An error
            // rather than `unreachable!` because a provisioning tool that
            // panics on a plan file is worse than one that explains itself.
            other => return Err(step.at.err(format!("`{other}` is not an action"))),
        };
        Ok(Some(action))
    }
}

/// One field of an action body as the action sees it. `raw: true` means the
/// body is not a template (spec §4): it still reaches the action, just as
/// written. `None` is a field that would not render; the expander's sweep has
/// already reported it.
pub(crate) fn rendered(engine: &Engine, ctx: &Value, raw: bool, src: &str) -> Option<String> {
    if raw {
        Some(src.to_string())
    } else {
        engine.render(src, ctx).ok()
    }
}

// ── typed actions ─────────────────────────────────────────────────────────

/// What a typed action did, or would do.
///
/// `shell`, `cmd` and `assert` report through an exit code, which is why they
/// run through the runner's argv path. Typed actions do their own work and
/// have to say what they did in their own words.
pub(crate) enum Effect {
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
pub(crate) struct Ctx<'a> {
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
    /// Spec §8, D19: the run's `--deadline` as an absolute instant, so every
    /// command this action runs can be clamped against what is *now* left.
    /// `timeout` above was computed once, before the action's first command,
    /// and a typed action runs several — a `pkg` step queries, installs and
    /// queries again. Without reclamping, each one got the whole remaining
    /// budget in turn and the step overran the deadline by a multiple of it.
    pub deadline: Option<Instant>,
    pub env: &'a BTreeMap<String, String>,
    /// What the last command actually got and whose clock gave it, for the
    /// wording of a kill. The clamp above means the second command of an
    /// action is bound by less than `timeout`, and a message that still
    /// named `timeout` would name a number no command was given — the same
    /// fault §8 names a step timeout the file does not contain to avoid.
    ///
    /// Both halves, not just the duration: a step with its own `timeout:`
    /// enters unbound by the deadline and can be clamped by it on a later
    /// command, and reading the duration fresh while the flag stayed at what
    /// the first command saw worded a deadline kill as a step timeout —
    /// after a number neither the file nor `--deadline` contains.
    pub last: std::cell::Cell<(Duration, bool)>,
}

impl Ctx<'_> {
    pub(crate) fn as_root(&self, argv: &[&str]) -> std::io::Result<process::Raw> {
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
    /// What this command gets: the lesser of the step's ceiling and the time
    /// the run has left, and whether the clock is the smaller. `None` means
    /// the run has none left at all.
    fn budget(&self) -> Option<(Duration, bool)> {
        process::clamp(self.deadline, self.timeout)
    }

    pub(crate) fn exec(&self, argv: &[&str], as_root: bool) -> std::io::Result<process::Raw> {
        // The clock ran out while an earlier command of this same action was
        // running. Answered as a kill rather than as an error, because that
        // is what happened: every caller already reports a `TimedOut` command
        // the way §8 wants, and inventing a second shape for the same event
        // would be a second wording for one thing.
        let Some((timeout, deadline_bound)) = self.budget() else {
            // Nothing left at all is the deadline's doing by definition:
            // the step's own ceiling cannot reach zero mid-action.
            self.last.set((Duration::ZERO, true));
            return Ok(process::Raw {
                rc: 124,
                stdout: Vec::new(),
                stderr: Vec::new(),
                how: process::How::TimedOut,
            });
        };
        self.last.set((timeout, deadline_bound));
        let argv: Vec<String> = argv.iter().map(|s| (*s).to_string()).collect();
        let (argv, stdin) = if as_root {
            // The step's own `env` keys, exactly as the shell path passes
            // them (spec §7). Without this a `file` step with `sudo` and
            // `env` loses its keys where a `shell` step with the same two
            // modifiers keeps them.
            let keys: Vec<String> = self.env.keys().cloned().collect();
            (self.escalate.wrap(argv, &keys), self.escalate.stdin())
        } else {
            (argv, None)
        };
        process::capture(&process::Spawn {
            argv: &argv,
            cwd: None,
            env: self.env,
            stdin: stdin.as_deref(),
            timeout,
            stream: false,
        })
    }

    /// Run a command that does work. `None` means it succeeded.
    ///
    /// The message is the **first** non-empty stderr line, not the last:
    /// systemd ends with "See `systemctl status ...`", which is a pointer,
    /// not a reason. The whole stderr goes to the detail, so the failure
    /// block shows what happened.
    pub(crate) fn perform(&self, argv: &[&str], as_root: bool) -> Option<Effect> {
        let got = match self.exec(argv, as_root) {
            Ok(g) => g,
            Err(e) => {
                return Some(Effect::fail(format!(
                    "cannot run {}: {e}",
                    command_name(argv)
                )));
            }
        };
        if let Some(bad) = self.stopped(&got) {
            return Some(bad);
        }
        if got.rc != 0 {
            let (why, detail) = failure_reason(&got);
            return Some(Effect::Failed {
                msg: format!("{} failed: {why}", command_name(argv)),
                detail,
                interrupted: false,
            });
        }
        None
    }

    /// The wording the shell path uses for a command that did not get to
    /// finish, so a `file` step and a `shell` step read the same when the
    /// same thing happened to them.
    pub(crate) fn stopped(&self, got: &process::Raw) -> Option<Effect> {
        match got.how {
            process::How::Exited => None,
            process::How::TimedOut => Some(Effect::Failed {
                msg: {
                    let (got, deadline_bound) = self.last.get();
                    if deadline_bound && got.is_zero() {
                        // The clock was already out when this command was
                        // reached, so `exec` declined to spawn it. There is
                        // no wait to report: reporting one read `deadline
                        // exceeded after 0ms`, a duration nobody waited and
                        // the exact wording the shell path rejects
                        // (`tests/apply.rs`). Its two siblings name the event
                        // instead — `deadline exceeded before attempt 2/3`
                        // and `before the step ran` — and this is the third
                        // (#56).
                        //
                        // Both halves of the guard: a step may declare
                        // `timeout: 0s`, which spawns and is killed at once,
                        // and that is a step timeout the file does contain.
                        // Only `exec`'s branch sets a zero duration while
                        // also blaming the deadline.
                        "deadline exceeded before the command ran".to_string()
                    } else {
                        format!(
                            "{} after {}",
                            if deadline_bound {
                                "deadline exceeded"
                            } else {
                                "timed out"
                            },
                            crate::output::event::human(got)
                        )
                    }
                },
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
    pub(crate) fn reads_as_root(&self) -> bool {
        self.sudo
    }
}

impl Effect {
    /// A failure with no captured output behind it.
    pub(crate) fn fail(msg: impl Into<String>) -> Effect {
        Effect::Failed {
            msg: msg.into(),
            detail: String::new(),
            interrupted: false,
        }
    }
}

/// Why a command failed, and the output to show under it.
///
/// stderr when it says anything. winget does not: it prints its errors to
/// stdout, after a progress spinner drawn with `\r`, so a failed `winget
/// uninstall` read `winget failed: failed` with nothing under it. Its last
/// stdout line is the error, which is where a tool that reports on stdout
/// puts it. The exit code when neither stream says anything at all.
fn failure_reason(got: &process::Raw) -> (String, String) {
    let stderr = got.stderr_text();
    if let Some(first) = stderr.lines().map(str::trim).find(|l| !l.is_empty()) {
        return (first.to_string(), stderr.trim().to_string());
    }
    let stdout = String::from_utf8_lossy(&got.stdout);
    // What a terminal would show of each line: the text after its last `\r`.
    let shown = |l: &'_ str| l.rsplit('\r').next().unwrap_or(l).trim().to_string();
    let last = stdout.lines().map(shown).rfind(|l| !l.is_empty());
    match last {
        Some(why) => (why, stdout.trim().to_string()),
        None => (format!("exit {}", got.rc), String::new()),
    }
}

/// The command a failure should be named after. `env VAR=… prog` is a way of
/// setting the environment, not a program anyone means to read about.
fn command_name<'a>(argv: &[&'a str]) -> &'a str {
    let mut i = 0;
    if argv.first() == Some(&"env") {
        i = 1;
        while argv.get(i).is_some_and(|a| a.contains('=')) {
            i += 1;
        }
    }
    argv.get(i).copied().unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::sudo::Sudo;
    use std::time::Instant;

    fn raw(rc: i32, stdout: &str, stderr: &str) -> process::Raw {
        process::Raw {
            rc,
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
            how: process::How::Exited,
        }
    }

    // What `winget uninstall` printed on a Windows host, spinner and all,
    // with nothing on stderr. It read `winget failed: failed`.
    #[test]
    fn a_failure_reported_on_stdout_names_its_last_line() {
        let (why, detail) = failure_reason(&raw(
            -1_978_335_107,
            "Found jq [jqlang.jq]\n\r   - \r   \\ \rThe package installed for user scope \
             cannot be uninstalled when running with administrator privileges.\r\n",
            "",
        ));
        assert_eq!(
            why,
            "The package installed for user scope cannot be uninstalled when running \
             with administrator privileges."
        );
        assert!(detail.starts_with("Found jq"), "{detail}");
    }

    #[test]
    fn stderr_still_wins_when_it_says_something() {
        let (why, detail) = failure_reason(&raw(1, "progress\n", "\nE: no such package\n"));
        assert_eq!(why, "E: no such package");
        assert_eq!(detail, "E: no such package");
    }

    #[test]
    fn a_silent_failure_names_its_exit_code() {
        let (why, detail) = failure_reason(&raw(3, " \n", ""));
        assert_eq!(why, "exit 3");
        assert_eq!(detail, "");
    }

    /// A context whose run clock ran out a second ago.
    fn spent<'a>(escalate: &'a Sudo, env: &'a BTreeMap<String, String>) -> Ctx<'a> {
        Ctx {
            sudo: false,
            root_available: false,
            escalate,
            timeout: Duration::from_secs(600),
            deadline: Instant::now().checked_sub(Duration::from_secs(1)),
            env,
            last: std::cell::Cell::new((Duration::from_secs(600), false)),
        }
    }

    /// Spec §8: a typed action runs several commands — `pkg` queries,
    /// installs and queries again — and each is clamped against what is
    /// *now* left. When nothing is left, the command is not spawned at all
    /// and the answer is the one every caller already handles: a kill.
    ///
    /// The third of §8's stopped-before-anything-ran shapes, and the only
    /// one that needs no process. The window is microseconds wide from
    /// outside: the command before this one is itself killed at the
    /// deadline, so an end-to-end test would be racing that kill rather
    /// than reaching here.
    #[test]
    fn a_command_reached_with_no_budget_left_is_not_spawned() {
        let sudo = Sudo::none();
        let env = BTreeMap::new();
        let ctx = spent(&sudo, &env);
        // A command that would take thirty seconds and leave a mark if it
        // ever ran. It returns instantly, which is the assertion: nothing
        // was spawned.
        let started = Instant::now();
        let got = ctx
            .exec(&["sleep", "30"], false)
            .expect("the branch answers without spawning");
        assert!(started.elapsed() < Duration::from_secs(1));
        // `124` and `TimedOut` together: the shape every caller already
        // reads for a command the clock killed, rather than a second shape
        // for the same event.
        assert_eq!(got.rc, 124);
        assert_eq!(got.how, process::How::TimedOut);
        assert!(got.stdout.is_empty());
        assert!(got.stderr.is_empty());
    }

    /// What the step then says. It names the event rather than a duration,
    /// the way `deadline exceeded before attempt 2/3` and `before the step
    /// ran` do. It used to say `deadline exceeded after 0ms` — right about
    /// whose clock, wrong about the wait, since no command ran (#56).
    #[test]
    fn the_wording_of_a_command_that_was_never_spawned() {
        let sudo = Sudo::none();
        let env = BTreeMap::new();
        let ctx = spent(&sudo, &env);
        let got = ctx.exec(&["sleep", "30"], false).expect("answered");
        assert_eq!(ctx.last.get(), (Duration::ZERO, true));
        let Some(Effect::Failed { msg, .. }) = ctx.stopped(&got) else {
            panic!("a killed command is a failure")
        };
        assert_eq!(msg, "deadline exceeded before the command ran");
    }

    /// The same context with time left spawns for real and records what the
    /// command was given, which is what a later kill's message names.
    #[test]
    fn a_command_with_budget_left_records_what_it_got() {
        let sudo = Sudo::none();
        let env = BTreeMap::new();
        let mut ctx = spent(&sudo, &env);
        ctx.deadline = Some(Instant::now() + Duration::from_secs(30));
        let got = ctx
            .exec(&[if cfg!(windows) { "cmd" } else { "true" }], false)
            .expect("a command that exists");
        assert_eq!(got.how, process::How::Exited);
        let (left, deadline_bound) = ctx.last.get();
        assert!(left <= Duration::from_secs(30));
        assert!(
            deadline_bound,
            "the run's clock was the smaller and the wording has to say so"
        );
        assert!(
            ctx.stopped(&got).is_none(),
            "a command that exited is not a kill"
        );
    }
}
