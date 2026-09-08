//! `service` — ensure a service state under systemd or launchd. Spec §6.6.
//!
//! The backend is an enum with four operations, and nothing above it branches
//! on which one it is: the verdict below asks "is it active", "is it
//! enabled", "make it so", and never learns whether it is talking to
//! `systemctl` or `launchctl`. A later init system is one more variant and
//! four more argv lines, not a module.

use super::{Ctx, Effect};
use crate::config::model::Step;
use crate::error::Result;
use crate::template::Engine;
use crate::yaml::N;
use minijinja::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Systemd { user: bool },
    Launchd { user: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Want {
    Started,
    Stopped,
    Restarted,
    Reloaded,
}

impl Want {
    fn parse(name: &str, at: N<'_>) -> Result<Want> {
        Ok(match name {
            "started" => Want::Started,
            "stopped" => Want::Stopped,
            "restarted" => Want::Restarted,
            "reloaded" => Want::Reloaded,
            other => {
                return Err(at
                    .err(format!("unknown service state `{other}`"))
                    .with_note("one of: started, stopped, restarted, reloaded"));
            }
        })
    }

    /// Spec §6.6: `restarted` and `reloaded` have no before-state to compare
    /// against, so they are always changed.
    fn always_changes(self) -> bool {
        matches!(self, Want::Restarted | Want::Reloaded)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spec {
    pub name: String,
    pub state: Option<Want>,
    pub enabled: Option<bool>,
    pub backend: Backend,
}

// ── parsing ───────────────────────────────────────────────────────────────

pub fn parse(step: &Step<'_>, engine: &Engine, ctx: &Value, raw: bool) -> Result<Option<Spec>> {
    let body = step.body;
    let render = |src: &str| -> Option<String> {
        if raw {
            Some(src.to_string())
        } else {
            engine.render(src, ctx).ok()
        }
    };

    let Some(name_at) = body.get("name") else {
        return Err(body.err("`service` requires `name`"));
    };
    let Some(name) = render(name_at.as_str()?) else {
        return Ok(None);
    };

    let state = match body.get("state") {
        Some(n) => {
            let Some(text) = render(n.as_str()?) else {
                return Ok(None);
            };
            Some(Want::parse(&text, n)?)
        }
        None => None,
    };
    let enabled = match body.get("enabled") {
        Some(n) => Some(n.as_bool()?),
        None => None,
    };

    let scope_at = body.get("scope");
    let user = match scope_at {
        Some(n) => {
            let Some(text) = render(n.as_str()?) else {
                return Ok(None);
            };
            match text.as_str() {
                "system" => false,
                "user" => true,
                other => {
                    return Err(n
                        .err(format!("unknown scope `{other}`"))
                        .with_note("one of: system, user"));
                }
            }
        }
        None => false,
    };

    // Spec §6.6: a user-scope unit belongs to the invoking user's manager.
    // `sudo` would talk to root's, which is a different manager entirely, so
    // the two together mean opposite things.
    let sudo = step
        .mods
        .sudo
        .map(|n| n.as_bool().unwrap_or(false))
        .unwrap_or(false);
    if user && sudo {
        return Err(step
            .at
            .err("`scope: user` and `sudo: true` mean opposite things")
            .with_note("a user unit belongs to the invoking user's manager, not root's"));
    }

    // The backend follows the machine, so the error names the fact that
    // decided it rather than a guess about the plan.
    let os = ctx
        .get_attr("os")
        .ok()
        .map(|v| v.to_string())
        .unwrap_or_default();
    let backend = match os.as_str() {
        "linux" => Backend::Systemd { user },
        "darwin" => Backend::Launchd { user },
        "windows" => {
            return Err(body
                .err("`service` is not supported on Windows")
                .with_note("use `shell` with the powershell interpreter"));
        }
        other => {
            return Err(body
                .err(format!("`service` has no backend for os `{other}`"))
                .with_note("systemd on linux, launchd on darwin — the `os` fact decides"));
        }
    };

    if state.is_none() && enabled.is_none() {
        return Err(body
            .err("`service` needs `state` or `enabled`")
            .with_note("otherwise the step declares nothing"));
    }

    Ok(Some(Spec {
        name,
        state,
        enabled,
        backend,
    }))
}

// ── the four operations ───────────────────────────────────────────────────

impl Backend {
    /// `None` only when the command could not be run at all. An unknown or
    /// inactive unit is `Some(false)`: `is-active` exits 3 for those, which
    /// is an answer. The verdict treats `None` as "not as declared".
    fn is_active(&self, name: &str, ctx: &Ctx<'_>) -> Option<bool> {
        match self {
            Backend::Systemd { user } => {
                let got = ctx
                    .exec(&["systemctl", scope_flag(*user), "is-active", name], false)
                    .ok()?;
                Some(got.rc == 0)
            }
            Backend::Launchd { user } => {
                let got = ctx
                    .exec(&["launchctl", "print", &domain(*user, name)], false)
                    .ok()?;
                Some(got.rc == 0)
            }
        }
    }

    fn is_enabled(&self, name: &str, ctx: &Ctx<'_>) -> Option<bool> {
        match self {
            Backend::Systemd { user } => {
                let got = ctx
                    .exec(&["systemctl", scope_flag(*user), "is-enabled", name], false)
                    .ok()?;
                Some(got.rc == 0)
            }
            // `print` reports loaded-ness, not enablement, so a unit that is
            // enabled but not yet loaded would read disabled forever — the
            // step would change on every run after `enable` had already
            // worked. `print-disabled` lists exactly the pair `enable` and
            // `disable` write, and takes a domain rather than a path.
            Backend::Launchd { user } => {
                let got = ctx
                    .exec(&["launchctl", "print-disabled", &domain_root(*user)], false)
                    .ok()?;
                let out = String::from_utf8_lossy(&got.stdout);
                let line = out.lines().find(|l| l.contains(&format!("\"{name}\"")))?;
                Some(!line.contains("disabled"))
            }
        }
    }

    fn set_state(&self, name: &str, want: Want, ctx: &Ctx<'_>) -> std::result::Result<(), Effect> {
        let root = ctx.sudo;
        match self {
            Backend::Systemd { user } => {
                let verb = match want {
                    Want::Started => "start",
                    Want::Stopped => "stop",
                    Want::Restarted => "restart",
                    Want::Reloaded => "reload",
                };
                run(ctx, &["systemctl", scope_flag(*user), verb, name], root)
            }
            Backend::Launchd { user } => {
                let d = domain(*user, name);
                match want {
                    Want::Started => run(ctx, &["launchctl", "kickstart", &d], root),
                    Want::Restarted | Want::Reloaded => {
                        run(ctx, &["launchctl", "kickstart", "-k", &d], root)
                    }
                    Want::Stopped => run(ctx, &["launchctl", "bootout", &d], root),
                }
            }
        }
    }

    fn set_enabled(&self, name: &str, on: bool, ctx: &Ctx<'_>) -> std::result::Result<(), Effect> {
        let root = ctx.sudo;
        match self {
            Backend::Systemd { user } => {
                let verb = if on { "enable" } else { "disable" };
                run(ctx, &["systemctl", scope_flag(*user), verb, name], root)
            }
            // `bootstrap` needs the path to a plist, which a `service` step
            // does not carry — the unit is deployed by a `file` or `template`
            // step that already knows where it went. `enable`/`disable` take
            // a service target and no path, so that is what this uses.
            Backend::Launchd { user } => {
                let verb = if on { "enable" } else { "disable" };
                run(ctx, &["launchctl", verb, &domain(*user, name)], root)
            }
        }
    }
}

/// `--user` or nothing. systemctl takes the flag before the verb.
fn scope_flag(user: bool) -> &'static str {
    if user { "--user" } else { "--system" }
}

fn domain(user: bool, name: &str) -> String {
    format!("{}/{name}", domain_root(user))
}

/// `gui/<uid>` or `system`. `$UID` is a shell variable and is not exported,
/// so `getuid` is the only source; `scope: user` with `sudo` is rejected at
/// parse time, so this is always the invoking user.
fn domain_root(user: bool) -> String {
    if user {
        format!("gui/{}", users_uid())
    } else {
        "system".to_string()
    }
}

#[cfg(unix)]
fn users_uid() -> String {
    (unsafe { libc::getuid() }).to_string()
}

#[cfg(not(unix))]
fn users_uid() -> String {
    String::new()
}

fn run(ctx: &Ctx<'_>, argv: &[&str], as_root: bool) -> std::result::Result<(), Effect> {
    match ctx.perform(argv, as_root) {
        Some(bad) => Err(bad),
        None => Ok(()),
    }
}

// ── the verdict, which never asks which backend it is ─────────────────────

impl Spec {
    pub fn plan(&self, ctx: &Ctx<'_>) -> Effect {
        self.reach(ctx, false)
    }

    pub fn apply(&self, ctx: &Ctx<'_>) -> Effect {
        self.reach(ctx, true)
    }

    fn reach(&self, ctx: &Ctx<'_>, act: bool) -> Effect {
        let mut changes = Vec::new();

        if let Some(want) = self.state {
            let needed = if want.always_changes() {
                true
            } else {
                let active = self.backend.is_active(&self.name, ctx);
                match want {
                    Want::Started => active != Some(true),
                    Want::Stopped => active != Some(false),
                    _ => true,
                }
            };
            if needed {
                changes.push(format!("{} {}", verb_of(want), self.name));
                if act && let Err(bad) = self.backend.set_state(&self.name, want, ctx) {
                    return bad;
                }
            }
        }

        if let Some(want) = self.enabled {
            let have = self.backend.is_enabled(&self.name, ctx);
            if have != Some(want) {
                changes.push(format!(
                    "{} {}",
                    if want { "enable" } else { "disable" },
                    self.name
                ));
                if act && let Err(bad) = self.backend.set_enabled(&self.name, want, ctx) {
                    return bad;
                }
            }
        }

        if changes.is_empty() {
            Effect::Ok
        } else {
            Effect::Changed(Some(changes.join("\n")))
        }
    }
}

fn verb_of(w: Want) -> &'static str {
    match w {
        Want::Started => "start",
        Want::Stopped => "stop",
        Want::Restarted => "restart",
        Want::Reloaded => "reload",
    }
}
