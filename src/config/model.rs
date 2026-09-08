//! The step model, parsed from spanned YAML. Spec §3 and §4.
//!
//! Parsing is structural only: it proves a step has exactly one action key and
//! no unknown keys, and it keeps every field as a spanned node so that a later
//! render or type error still points at the right line. Nothing is templated
//! here — that needs a scope, which arrives at expansion.

use crate::error::Result;
use crate::yaml::N;
use std::time::Duration;

pub const ACTION_KEYS: &[&str] = &[
    "shell", "cmd", "file", "template", "pkg", "service", "assert",
];
pub const STRUCTURAL_KEYS: &[&str] = &["vars", "vars_file", "import", "use"];
pub const MODIFIER_KEYS: &[&str] = &[
    "name",
    "when",
    "unless",
    "creates",
    "sudo",
    "timeout",
    "retry",
    "env",
    "cwd",
    "tags",
    "register",
    "changed_when",
    "failed_when",
    "raw",
    "props",
    "optional",
];

/// Every key a step may carry, for the unknown-key check.
pub fn all_step_keys() -> Vec<&'static str> {
    ACTION_KEYS
        .iter()
        .chain(STRUCTURAL_KEYS)
        .chain(MODIFIER_KEYS)
        .copied()
        .collect()
}

/// The fields each action's long form accepts (spec §6). `cmd` is a
/// sequence, not a map, and so has no entry.
pub fn action_body_keys(action: &str) -> Option<&'static [&'static str]> {
    Some(match action {
        "shell" => &["script", "interpreter", "login"],
        "file" => &[
            "path", "state", "content", "src", "mode", "owner", "group", "force",
        ],
        "template" => &["src", "dest", "mode", "owner", "group"],
        "pkg" => &["name", "names", "state", "manager", "cask", "update_cache"],
        "service" => &["name", "state", "enabled", "scope"],
        "assert" => &["command", "expr", "msg"],
        _ => return None,
    })
}

#[derive(Clone, Copy)]
pub struct Step<'a> {
    pub at: N<'a>,
    /// The one action or structural key this step carries.
    pub key: &'a str,
    pub body: N<'a>,
    pub mods: Mods<'a>,
}

impl<'a> Step<'a> {
    pub fn is_structural(&self) -> bool {
        STRUCTURAL_KEYS.contains(&self.key)
    }

    /// Used when `name` is absent or renders empty (spec §10).
    pub fn fallback_name(&self) -> String {
        let arg = match self.body.as_str() {
            Ok(s) => s.lines().next().unwrap_or("").trim().to_string(),
            Err(_) => self
                .body
                .keys()
                .ok()
                .and_then(|ks| ks.first().map(|(k, _)| k.to_string()))
                .or_else(|| {
                    self.body.as_seq().ok().and_then(|items| {
                        items
                            .first()
                            .and_then(|i| i.as_str().ok())
                            .map(String::from)
                    })
                })
                .unwrap_or_default(),
        };
        if arg.is_empty() {
            self.key.to_string()
        } else {
            let arg = truncate(&arg, 48);
            format!("{}: {arg}", self.key)
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max - 1).collect();
    format!("{head}…")
}

#[derive(Clone, Copy, Default)]
pub struct Mods<'a> {
    pub name: Option<N<'a>>,
    pub when: Option<N<'a>>,
    pub unless: Option<N<'a>>,
    pub creates: Option<N<'a>>,
    pub sudo: Option<N<'a>>,
    pub timeout: Option<N<'a>>,
    pub retry: Option<N<'a>>,
    pub env: Option<N<'a>>,
    pub cwd: Option<N<'a>>,
    pub tags: Option<N<'a>>,
    pub register: Option<N<'a>>,
    pub changed_when: Option<N<'a>>,
    pub failed_when: Option<N<'a>>,
    pub raw: Option<N<'a>>,
    pub props: Option<N<'a>>,
    pub optional: Option<N<'a>>,
}

/// Parse the list of steps at the top of a plan file.
///
/// A malformed step does not stop the others: the gate is that *every*
/// validation error carries a position, which means reporting all of them.
pub fn parse_steps<'a>(root: N<'a>) -> Result<(Vec<Step<'a>>, Vec<crate::error::Diag>)> {
    let items = root.as_seq().map_err(|_| {
        root.err(format!("a plan is a list of steps, found {}", root.kind()))
            .with_note(
                "a component is a mapping with `props:` and `steps:` and is entered with `use:`",
            )
    })?;
    let mut steps = Vec::new();
    let mut errors = Vec::new();
    for item in items {
        match parse_step(item) {
            Ok(s) => steps.push(s),
            Err(d) => errors.push(d),
        }
    }
    Ok((steps, errors))
}

pub fn parse_step<'a>(at: N<'a>) -> Result<Step<'a>> {
    let keys = at
        .as_map()
        .map_err(|_| at.err(format!("a step is a mapping, found {}", at.kind())))
        .and_then(|_| at.keys())?;

    at.deny_unknown_keys(&all_step_keys(), "a step")?;

    let subjects: Vec<(&str, N<'a>)> = keys
        .iter()
        .copied()
        .filter(|(k, _)| ACTION_KEYS.contains(k) || STRUCTURAL_KEYS.contains(k))
        .collect();

    let (key, _key_at) = match subjects.len() {
        1 => subjects[0],
        0 => {
            return Err(at.err("step has no action").with_note(format!(
                "expected one of: {}",
                all_subject_keys().join(", ")
            )));
        }
        _ => {
            let names: Vec<&str> = subjects.iter().map(|(k, _)| *k).collect();
            // Point at the second one: the first is likely what was meant.
            return Err(subjects[1]
                .1
                .err(format!(
                    "step has {} action keys: {}",
                    names.len(),
                    names.join(", ")
                ))
                .with_note("a step carries exactly one action or structural key"));
        }
    };

    let body = at.require(key)?;
    let mut mods = Mods::default();
    for (name, key_node) in keys {
        if !MODIFIER_KEYS.contains(&name) {
            continue;
        }
        let v = at.require(name)?;
        let _ = key_node;
        match name {
            "name" => mods.name = Some(v),
            "when" => mods.when = Some(v),
            "unless" => mods.unless = Some(v),
            "creates" => mods.creates = Some(v),
            "sudo" => mods.sudo = Some(v),
            "timeout" => mods.timeout = Some(v),
            "retry" => mods.retry = Some(v),
            "env" => mods.env = Some(v),
            "cwd" => mods.cwd = Some(v),
            "tags" => mods.tags = Some(v),
            "register" => mods.register = Some(v),
            "changed_when" => mods.changed_when = Some(v),
            "failed_when" => mods.failed_when = Some(v),
            "raw" => mods.raw = Some(v),
            "props" => mods.props = Some(v),
            "optional" => mods.optional = Some(v),
            _ => unreachable!("modifier list and match are the same set"),
        }
    }

    Ok(Step {
        at,
        key,
        body,
        mods,
    })
}

fn all_subject_keys() -> Vec<&'static str> {
    ACTION_KEYS.iter().chain(STRUCTURAL_KEYS).copied().collect()
}

// ── modifier types ────────────────────────────────────────────────────────

/// `30s`, `5m`, `1h`. Spec §4.
pub fn parse_duration(at: N<'_>) -> Result<Duration> {
    let raw = at.as_scalar_string()?;
    let s = raw.trim();
    let bad = || {
        at.err(format!("`{s}` is not a duration"))
            .with_note("use a number and a unit: 30s, 5m, 1h")
    };
    let (num, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit()).ok_or_else(bad)?);
    let n: u64 = num.parse().map_err(|_| bad())?;
    let secs = match unit {
        "s" => n,
        "m" => n * 60,
        "h" => n * 3600,
        _ => return Err(bad()),
    };
    Ok(Duration::from_secs(secs))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Retry {
    pub attempts: u32,
    pub delay: Duration,
}

pub fn parse_retry(at: N<'_>) -> Result<Retry> {
    at.deny_unknown_keys(&["attempts", "delay"], "`retry`")?;
    let attempts_at = at.require("attempts")?;
    let attempts: u32 = attempts_at
        .as_scalar_string()?
        .parse()
        .map_err(|_| attempts_at.err("`attempts` must be a whole number"))?;
    if attempts < 1 {
        return Err(attempts_at.err("`attempts` must be at least 1"));
    }
    let delay = match at.get("delay") {
        Some(d) => parse_duration(d)?,
        None => Duration::ZERO,
    };
    Ok(Retry { attempts, delay })
}

/// Check the modifier combinations that are wrong regardless of scope. The
/// rest wait for expansion, when values are rendered.
pub fn check_modifiers(step: &Step<'_>) -> Result<()> {
    let m = &step.mods;

    if let Some(n) = m.timeout {
        parse_duration(n)?;
    }
    if let Some(n) = m.retry {
        parse_retry(n)?;
    }
    if let Some(n) = m.sudo {
        n.as_bool()?;
    }
    if let Some(n) = m.raw {
        n.as_bool()?;
    }
    if let Some(n) = m.optional {
        n.as_bool()?;
    }
    if let Some(n) = m.tags {
        n.as_str_or_seq()?;
    }
    if let Some(n) = m.env {
        n.as_map()?;
    }

    if let Some(n) = m.register {
        let name = n.as_str()?;
        if !is_identifier(name) {
            return Err(n
                .err(format!("`register: {name}` is not a valid identifier"))
                .with_note("letters, digits and underscore, not starting with a digit"));
        }
    }

    // `props` belongs to `use`; `optional` belongs to `vars_file`. Silently
    // ignoring a misplaced modifier is how a typo becomes a two-hour debug.
    if let Some(n) = m.props.filter(|_| step.key != "use") {
        return Err(n.err(format!(
            "`props` is only valid on `use`, not on `{}`",
            step.key
        )));
    }
    if let Some(n) = m.optional.filter(|_| step.key != "vars_file") {
        return Err(n.err(format!(
            "`optional` is only valid on `vars_file`, not on `{}`",
            step.key
        )));
    }

    // Spec §4: sudo is root or nothing, and Windows has no sudo (D8).
    if let Some(n) = m
        .sudo
        .filter(|n| cfg!(target_os = "windows") && n.as_bool().unwrap_or(false))
    {
        return Err(n
            .err("`sudo` is not supported on Windows")
            .with_note("run provision from an elevated PowerShell prompt instead"));
    }

    Ok(())
}

fn is_identifier(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with(|c: char| c.is_ascii_digit())
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The expression fields, which compile as expressions rather than templates.
pub fn expression_fields<'a>(step: &Step<'a>) -> Vec<N<'a>> {
    [
        step.mods.when,
        step.mods.changed_when,
        step.mods.failed_when,
    ]
    .into_iter()
    .flatten()
    .collect()
}
