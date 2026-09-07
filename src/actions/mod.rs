//! The rendered actions. Phase 1 ships the three that reduce to "run a
//! command and judge it by its exit code": `shell`, `cmd`, `assert`.
//!
//! They share one implementation because they genuinely are one — the
//! differences are which argv gets built and what a success means. Phase 2's
//! actions (`file`, `template`, `pkg`, `service`) each need real state
//! inspection and get their own modules then.

use crate::error::Result;
use crate::yaml::N;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Shell { script: String, interpreter: Interpreter, login: bool },
    Cmd(Vec<String>),
    Assert { command: Option<String>, expr: Option<String>, msg: Option<String> },
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
}
