//! `defaults` — ensure a macOS preference key holds a value. Spec §6.10.
//!
//! `defaults` stores values by type, and a `true` written as a string is a
//! different key from one written as a bool. That is why `type` is required
//! and why the comparison is typed: `defaults read` prints a bool as `1`, so
//! comparing its output to the word `true` would report `changed` forever.
//!
//! The compare is a pure function over the text `defaults read` prints, which
//! is what lets it be tested on a machine that has no `defaults` at all.

use super::{Ctx, Effect};
use crate::config::model::Step;
use crate::error::Result;
use crate::template::Engine;
use crate::yaml::N;
use minijinja::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Type {
    Bool,
    Int,
    Float,
    Str,
}

impl Type {
    fn parse(name: &str, at: N<'_>) -> Result<Type> {
        Ok(match name {
            "bool" => Type::Bool,
            "int" => Type::Int,
            "float" => Type::Float,
            "string" => Type::Str,
            other => {
                return Err(at
                    .err(format!("unknown defaults type `{other}`"))
                    .with_note(
                        "one of: bool, int, float, string — `array` and `dict` \
                         are deferred, keep those in shell",
                    ));
            }
        })
    }

    /// The flag `defaults write` takes.
    fn flag(self) -> &'static str {
        match self {
            Type::Bool => "-bool",
            Type::Int => "-int",
            Type::Float => "-float",
            Type::Str => "-string",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Spec {
    pub domain: String,
    pub key: String,
    pub kind: Type,
    pub value: String,
    pub current_host: bool,
    /// Taken from the `os` fact at parse time. §6.10 wants a verdict, not a
    /// parse error: apply fails and plan says `unknown`.
    pub macos: bool,
}

// ── parsing ───────────────────────────────────────────────────────────────

pub(crate) fn parse(
    step: &Step<'_>,
    engine: &Engine,
    ctx: &Value,
    raw: bool,
) -> Result<Option<Spec>> {
    let body = step.body;
    let render = |src: &str| -> Option<String> {
        if raw {
            Some(src.to_string())
        } else {
            engine.render(src, ctx).ok()
        }
    };

    let field = |name: &str| -> Result<Option<String>> {
        let Some(n) = body.get(name) else {
            return Err(body.err(format!("`defaults` requires `{name}`")));
        };
        Ok(render(&n.as_scalar_string()?))
    };
    let (Some(domain), Some(key)) = (field("domain")?, field("key")?) else {
        return Ok(None);
    };

    let Some(type_at) = body.get("type") else {
        return Err(body.err("`defaults` requires `type`").with_note(
            "`defaults` stores values by type: a `true` written as a string \
             is a different key from one written as a bool",
        ));
    };
    let Some(type_name) = render(type_at.as_str()?) else {
        return Ok(None);
    };
    let kind = Type::parse(&type_name, type_at)?;

    let Some(value_at) = body.get("value") else {
        return Err(body.err("`defaults` requires `value`"));
    };
    let Some(value) = render(&value_at.as_scalar_string()?) else {
        return Ok(None);
    };
    let value = normalize(kind, value.trim(), value_at)?;

    let current_host = match body.get("current_host") {
        Some(n) => n.as_bool()?,
        None => false,
    };

    // Spec §6.10: a root write lands in root's preferences, which is a
    // different user's settings and never what the step meant.
    if step.mods.sudo.is_some_and(|n| n.as_bool().unwrap_or(false)) {
        return Err(step
            .at
            .err("`defaults` must not run under `sudo`")
            .with_note("a root write lands in /var/root's preferences, not yours"));
    }

    let macos = ctx.get_attr("os").is_ok_and(|v| v.to_string() == "darwin");

    Ok(Some(Spec {
        domain,
        key,
        kind,
        value,
        current_host,
        macos,
    }))
}

/// The value as `defaults write` should receive it, rejecting what the type
/// cannot mean. YAML's own typing is not enough: `value: yes` is a bool to
/// YAML and a string to anyone reading the plan, so the declared `type` is
/// what decides and a mismatch is said at parse time rather than becoming a
/// key that never converges.
// The parse errors say "invalid digit"; `bad` says what the type meant.
#[expect(
    clippy::map_err_ignore,
    reason = "the replacement diagnostic restates the cause"
)]
fn normalize(kind: Type, text: &str, at: N<'_>) -> Result<String> {
    let bad = |want: &str| {
        at.err(format!("`{text}` is not {want}"))
            .with_note("the step's `type` decides how the value is read")
    };
    Ok(match kind {
        Type::Bool => match text {
            "true" | "yes" | "1" => "true".to_string(),
            "false" | "no" | "0" => "false".to_string(),
            _ => return Err(bad("a bool")),
        },
        Type::Int => {
            text.parse::<i64>().map_err(|_| bad("an integer"))?;
            text.to_string()
        }
        Type::Float => {
            let n: f64 = text.parse().map_err(|_| bad("a number"))?;
            if !n.is_finite() {
                return Err(bad("a finite number"));
            }
            text.to_string()
        }
        Type::Str => text.to_string(),
    })
}

// ── the compare, which never runs a command ───────────────────────────────

/// What `defaults read` prints, against what the step declared.
///
/// `have` is the raw stdout. `None` is a key that does not exist, which is
/// always a change.
#[expect(
    clippy::float_cmp,
    reason = "the question is whether the stored preference is this value, not whether it is near it: a tolerance would call a drifted key converged and stop the step correcting it"
)]
pub(crate) fn matches(kind: Type, have: Option<&str>, want: &str) -> bool {
    let Some(have) = have.map(str::trim) else {
        return false;
    };
    match kind {
        // `defaults read` prints a bool as 1 or 0, never as true or false.
        Type::Bool => have == if want == "true" { "1" } else { "0" },
        Type::Int => match (have.parse::<i64>(), want.parse::<i64>()) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        },
        // Numerically, so `0.001` and `1e-3` are the same value and the step
        // does not converge forever on how it was written.
        Type::Float => match (have.parse::<f64>(), want.parse::<f64>()) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        },
        Type::Str => have == want,
    }
}

// ── the verdict ───────────────────────────────────────────────────────────

impl Spec {
    pub(crate) fn plan(&self, ctx: &Ctx<'_>) -> Effect {
        self.reach(ctx, false)
    }

    pub(crate) fn apply(&self, ctx: &Ctx<'_>) -> Effect {
        self.reach(ctx, true)
    }

    fn reach(&self, ctx: &Ctx<'_>, act: bool) -> Effect {
        // Spec §6.10: the action does not skip itself. A plan that runs
        // `defaults` on Linux is a plan with a mistake in it, and saying so
        // at apply is more useful than a green line that did nothing.
        if !self.macos {
            if !act {
                return Effect::Unknown;
            }
            return Effect::fail("`defaults` runs on macOS only");
        }

        let have = self.read(ctx);
        if matches(self.kind, have.as_deref(), &self.value) {
            return Effect::Ok;
        }
        if act && let Some(bad) = ctx.perform(&self.write_argv(), false) {
            return bad;
        }
        Effect::Changed(Some(format!(
            "{} {} {} → {}",
            self.domain,
            self.key,
            have.as_deref().map_or("(unset)", str::trim),
            self.value
        )))
    }

    /// `None` when the key does not exist, which is what `defaults read`
    /// says by exiting non-zero.
    fn read(&self, ctx: &Ctx<'_>) -> Option<String> {
        let mut argv = vec!["defaults"];
        if self.current_host {
            argv.push("-currentHost");
        }
        argv.extend(["read", &self.domain, &self.key]);
        let got = ctx.exec(&argv, false).ok()?;
        (got.rc == 0).then(|| String::from_utf8_lossy(&got.stdout).into_owned())
    }

    fn write_argv(&self) -> Vec<&str> {
        let mut argv = vec!["defaults"];
        if self.current_host {
            argv.push("-currentHost");
        }
        argv.extend([
            "write",
            &self.domain,
            &self.key,
            self.kind.flag(),
            &self.value,
        ]);
        argv
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The strings below are what `defaults read` actually printed on the
    /// mac plans, collected while gating the twenty writes this action
    /// replaces. A bool comes back as 1 or 0; a float comes back as it was
    /// written; a string comes back bare, with a trailing newline.
    #[test]
    fn a_bool_reads_back_as_one_or_zero_not_as_a_word() {
        assert!(matches(Type::Bool, Some("1\n"), "true"));
        assert!(matches(Type::Bool, Some("0\n"), "false"));
        assert!(!matches(Type::Bool, Some("0\n"), "true"));
        // The mistake this action exists to stop making.
        assert!(!matches(Type::Bool, Some("true\n"), "true"));
    }

    #[test]
    fn a_missing_key_is_always_a_change() {
        assert!(!matches(Type::Bool, None, "false"));
        assert!(!matches(Type::Int, None, "0"));
        assert!(!matches(Type::Str, None, ""));
    }

    #[test]
    fn numbers_compare_numerically_and_strings_do_not() {
        assert!(matches(Type::Float, Some("0.001\n"), "0.001"));
        assert!(matches(Type::Float, Some("0.001\n"), "1e-3"));
        assert!(!matches(Type::Float, Some("0.002\n"), "0.001"));
        assert!(matches(Type::Int, Some("2\n"), "2"));
        assert!(!matches(Type::Int, Some("2\n"), "1"));
        // `askForPasswordDelay` is an int; a string compare would be fooled
        // by the same value written differently.
        assert!(matches(Type::Int, Some("0\n"), "0"));
        assert!(!matches(Type::Str, Some("0.001\n"), "1e-3"));
    }

    #[test]
    fn a_string_compares_as_it_was_printed() {
        assert!(matches(
            Type::Str,
            Some("/Users/aleh/Desktop\n"),
            "/Users/aleh/Desktop"
        ));
        assert!(matches(Type::Str, Some("png\n"), "png"));
        assert!(!matches(Type::Str, Some("png\n"), "jpg"));
    }

    #[test]
    fn a_plist_block_never_matches_a_scalar() {
        // The one array key in the fleet stays in shell (§6.10). If it ever
        // reached here it must read as a change, not as a match.
        let array = "(\n    \"/System/Library/CoreServices/Menu Extras/Clock.menu\"\n)\n";
        assert!(!matches(Type::Str, Some(array), "Clock.menu"));
    }
}
