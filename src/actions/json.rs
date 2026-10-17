//! `json` — ensure a JSON file *contains* what the plan declares, leaving
//! every other key as it is. Spec §6.11, D31.
//!
//! The one thing that makes this action different from `file` is the
//! predicate. `file` asserts that a file *is* something; this asserts that
//! merging a declaration into it changes nothing. That is a containment, and
//! it is what a file with two owners needs: `~/.claude/settings.json` is
//! deployed by a component and rewritten by the application, so an equality
//! discards whichever half it was not given (#54).
//!
//! Everything else is `file`'s, deliberately. Once the merged document is
//! known it is a `file::Spec` with computed content, which is what `template`
//! already is, so the sudo rules, the parent creation, the atomic write and
//! the mode comparison have one implementation and not two.

use super::{Ctx, Effect, file};
use crate::config::model::Step;
use crate::error::Result;
use crate::template::{Engine, expanduser, expanduser_in};
use crate::yaml::N;
use minijinja::Value;
use serde_json::Value as J;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Spec {
    pub path: String,
    /// The declared value, rendered at parse time. Always an object: there is
    /// nothing to merge into a target from an array or a scalar.
    declared: serde_json::Map<String, J>,
    /// `mode` only. `owner` and `group` are not fields of this action, so the
    /// trio arrives with two of them empty.
    meta: file::Metadata,
}

// ── parsing ───────────────────────────────────────────────────────────────

pub(crate) fn parse(
    step: &Step<'_>,
    engine: &Engine,
    ctx: &Value,
    raw: bool,
    defer_src: bool,
) -> Result<Option<Spec>> {
    let body = step.body;
    let render = |src: &str| super::rendered(engine, ctx, raw, src);

    let Some(path_at) = body.get("path") else {
        return Err(body.err("`json` requires `path`"));
    };
    let Some(path) = render(path_at.as_str()?) else {
        return Ok(None);
    };

    let Some(meta) = file::parse_metadata(step, engine, ctx, raw)? else {
        return Ok(None);
    };

    // `file`'s rule (§6.3), because it is the same choice: neither is an
    // error, both is an error positioned at the second.
    let declared = match (body.get("content"), body.get("src")) {
        (Some(_), Some(s)) => {
            return Err(s
                .err("`content` and `src` are mutually exclusive")
                .with_note("content is the keys to merge; src is a .json file holding them"));
        }
        (None, None) => {
            return Err(body
                .err("`json` needs `content` or `src`")
                .with_note("content: the keys this plan owns · src: a .json file holding them"));
        }
        (Some(c), None) => match from_yaml(c, engine, ctx, raw)? {
            Some(v) => v,
            None => return Ok(None),
        },
        (None, Some(s)) => {
            let Some(rel) = render(s.as_str()?) else {
                return Ok(None);
            };
            let from = crate::config::load::resolve(s.file, &expanduser(&rel));
            // D18, exactly as `file` reads it: on a walk that runs nothing, a
            // source an earlier command has yet to build is unresolved rather
            // than missing, and the declared keys are the whole reason to
            // parse this step.
            if defer_src && !from.exists() {
                return Ok(None);
            }
            let text = std::fs::read_to_string(&from)
                .map_err(|e| s.err(format!("cannot read {}: {e}", from.display())))?;
            let parsed: J = serde_json::from_str(&text)
                .map_err(|e| s.err(format!("{} is not valid JSON: {e}", from.display())))?;
            match parsed {
                J::Object(m) => m,
                other => {
                    return Err(s
                        .err(format!(
                            "{} is {}, not a JSON object",
                            from.display(),
                            shape(&other)
                        ))
                        .with_note("a merge declares keys, so the source has to have keys"));
                }
            }
        }
    };

    Ok(Some(Spec {
        path: expanduser_in(ctx, &path),
        declared,
        meta,
    }))
}

/// The declared keys, written as YAML in the plan and read as data.
///
/// Every string in the tree is templated and the shape is not (§6.11): a
/// `{{ … }}` may produce a value, never a key structure, which is what keeps
/// a plan readable without rendering it first.
fn from_yaml(
    at: N<'_>,
    engine: &Engine,
    ctx: &Value,
    raw: bool,
) -> Result<Option<serde_json::Map<String, J>>> {
    let J::Object(mut map) = at.to_json()? else {
        return Err(at
            .err(format!("`content` is {}, not a mapping", at.kind()))
            .with_note("a merge declares keys, so content has to have keys"));
    };
    if !render_strings(&mut map, engine, ctx, raw) {
        return Ok(None);
    }
    Ok(Some(map))
}

/// Render every string in the tree in place. `false` means one of them would
/// not render — the expander's own sweep has already said which, with a
/// position, so this stays silent (`Action::parse`).
fn render_strings(
    map: &mut serde_json::Map<String, J>,
    engine: &Engine,
    ctx: &Value,
    raw: bool,
) -> bool {
    map.values_mut().all(|v| render_value(v, engine, ctx, raw))
}

fn render_value(v: &mut J, engine: &Engine, ctx: &Value, raw: bool) -> bool {
    match v {
        J::String(s) => match super::rendered(engine, ctx, raw, s) {
            Some(text) => {
                *s = text;
                true
            }
            None => false,
        },
        J::Array(items) => items.iter_mut().all(|i| render_value(i, engine, ctx, raw)),
        J::Object(m) => render_strings(m, engine, ctx, raw),
        _ => true,
    }
}

// ── the merge ─────────────────────────────────────────────────────────────

/// Spec §6.11's one rule: a declared leaf wins, an undeclared key is left
/// alone. Mappings are merged key by key; everything that is not a mapping —
/// arrays included — is a leaf and is replaced whole.
///
/// This is a fixpoint, which is the whole of the idempotency argument (D31):
/// merging the same declaration into an already-merged document writes the
/// leaf that is already there, so the second apply is `ok`.
fn merge(base: &mut serde_json::Map<String, J>, declared: &serde_json::Map<String, J>) {
    for (k, want) in declared {
        match (base.get_mut(k), want) {
            (Some(J::Object(have)), J::Object(want)) => merge(have, want),
            _ => {
                base.insert(k.clone(), want.clone());
            }
        }
    }
}

/// One line per leaf that differs, keyed by its dotted path. Display only:
/// nothing parses these back, which is why a key containing a `.` is allowed
/// to read ambiguously here (§10).
fn deltas(
    have: &serde_json::Map<String, J>,
    want: &serde_json::Map<String, J>,
    at: &str,
) -> Vec<String> {
    let mut out = Vec::new();
    for (k, new) in want {
        let path = if at.is_empty() {
            k.clone()
        } else {
            format!("{at}.{k}")
        };
        match (have.get(k), new) {
            (Some(J::Object(a)), J::Object(b)) => out.extend(deltas(a, b, &path)),
            (Some(old), _) if old == new => {}
            (Some(old), _) => out.push(format!("{path}: {} → {}", show(old), show(new))),
            (None, _) => out.push(format!("{path}: absent → {}", show(new))),
        }
    }
    out
}

/// A value as one line of JSON. `"opus"` keeps its quotes on purpose: it is
/// what distinguishes the string `1` from the number.
fn show(v: &J) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "?".to_string())
}

fn shape(v: &J) -> &'static str {
    match v {
        J::Null => "null",
        J::Bool(_) => "a boolean",
        J::Number(_) => "a number",
        J::String(_) => "a string",
        J::Array(_) => "an array",
        J::Object(_) => "an object",
    }
}

/// Two-space indent and a trailing newline, keys sorted at every depth —
/// sorted because `serde_json`'s map is ordered and keeping a file's own
/// order would mean an order-preserving map through the parser, which is a
/// dependency (§6.11). The first write reshuffles the target once; every
/// write after it is stable.
fn render_json(doc: &serde_json::Map<String, J>) -> String {
    let mut text = serde_json::to_string_pretty(doc).unwrap_or_else(|_| "{}".to_string());
    text.push('\n');
    text
}

// ── probe and apply ───────────────────────────────────────────────────────

impl Spec {
    pub(crate) fn plan(&self, ctx: &Ctx<'_>) -> Effect {
        self.reach(ctx, false)
    }

    pub(crate) fn apply(&self, ctx: &Ctx<'_>) -> Effect {
        self.reach(ctx, true)
    }

    fn reach(&self, ctx: &Ctx<'_>, act: bool) -> Effect {
        // `file`'s rule (§6.3): a target this run cannot read is a verdict
        // plan does not have, not a failure.
        if ctx.sudo && !ctx.root_available {
            return Effect::Unprobed;
        }
        let current = match file::probe(&self.path, ctx) {
            Ok(bytes) => bytes,
            Err(msg) => return Effect::fail(msg),
        };

        let (have, keep) = match &current {
            None => (serde_json::Map::new(), None),
            Some(bytes) => {
                let text = String::from_utf8_lossy(bytes);
                match serde_json::from_str::<J>(&text) {
                    Ok(J::Object(m)) => (m, Some(bytes)),
                    // Never overwritten. A file this action cannot read holds
                    // state provision did not put there, and preserving that
                    // state is the reason the action exists (§6.11).
                    Ok(other) => {
                        return Effect::fail(format!(
                            "{} is {}, not a JSON object",
                            self.path,
                            shape(&other)
                        ));
                    }
                    Err(e) => {
                        return Effect::fail(format!("{} is not valid JSON: {e}", self.path));
                    }
                }
            }
        };

        let mut merged = have.clone();
        merge(&mut merged, &self.declared);
        let lines = deltas(&have, &merged, "");

        // The verdict is on values, never on bytes (§6.11). Where the values
        // already agree, the content handed to `file` is the file's own, so a
        // target indented differently from what this action emits is left
        // alone instead of rewritten on every run — and what is left for
        // `file` to find is the mode.
        let content = match (lines.is_empty(), keep) {
            (true, Some(bytes)) => bytes.clone(),
            _ => render_json(&merged).into_bytes(),
        };

        let as_file = self
            .meta
            .clone()
            .into_file(self.path.clone(), content, None);
        let effect = if act {
            as_file.apply(ctx)
        } else {
            as_file.plan(ctx)
        };

        // `file` would report its own unified text diff, which would show the
        // reordering of a first write as though it were a change in what the
        // file says. Where the values differ this reports them instead; where
        // only the mode does, `file`'s delta is already the right answer.
        match (effect, lines.is_empty()) {
            (Effect::Changed(_), false) => Effect::Changed(Some(lines.join("\n"))),
            (other, _) => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn obj(v: serde_json::Value) -> serde_json::Map<String, J> {
        match v {
            J::Object(m) => m,
            other => panic!("the fixture is {}", shape(&other)),
        }
    }

    fn merged(base: serde_json::Value, declared: serde_json::Value) -> serde_json::Value {
        let mut b = obj(base);
        merge(&mut b, &obj(declared));
        J::Object(b)
    }

    #[test]
    fn an_undeclared_key_is_left_alone_at_every_depth() {
        assert_eq!(
            merged(
                json!({"model": "sonnet", "p": {"allow": ["a"], "n": 1}}),
                json!({"p": {"deny": ["b"]}})
            ),
            json!({"model": "sonnet", "p": {"allow": ["a"], "n": 1, "deny": ["b"]}})
        );
    }

    #[test]
    fn a_declared_leaf_wins_and_an_array_is_a_leaf() {
        assert_eq!(
            merged(json!({"t": ["old", "stale"]}), json!({"t": ["new"]})),
            json!({"t": ["new"]})
        );
        // A scalar replacing a mapping is the same rule read the other way:
        // the declaration is what the key holds afterwards.
        assert_eq!(
            merged(json!({"t": {"deep": 1}}), json!({"t": 5})),
            json!({"t": 5})
        );
        // And a mapping replacing a scalar, which is the case that would
        // panic if the merge assumed the shapes matched.
        assert_eq!(
            merged(json!({"t": 5}), json!({"t": {"deep": 1}})),
            json!({"t": {"deep": 1}})
        );
    }

    #[test]
    fn a_declared_null_is_a_value_and_not_a_removal() {
        // §6.11 has no removal, so `null` cannot be borrowed to mean one: it
        // is a legitimate JSON value and is written as itself.
        assert_eq!(
            merged(json!({"a": 1}), json!({"a": null})),
            json!({"a": null})
        );
    }

    /// D31's idempotency argument, as a test rather than a paragraph: merging
    /// the same declaration into an already-merged document changes nothing,
    /// which is why the second apply is `ok`.
    #[test]
    fn merging_twice_is_merging_once() {
        let base = json!({"keep": true, "p": {"allow": ["x"]}});
        let want = json!({"p": {"deny": ["y"]}, "model": "opus"});
        let once = merged(base, want.clone());
        assert_eq!(merged(once.clone(), want), once);
    }

    #[test]
    fn the_deltas_name_the_leaf_and_not_the_document() {
        let have = obj(json!({"model": "sonnet", "p": {"allow": ["x"], "deny": ["y"]}}));
        let want = obj(json!({"model": "opus", "p": {"allow": ["x"], "new": 1}}));
        assert_eq!(
            deltas(&have, &want, ""),
            vec![
                r#"model: "sonnet" → "opus""#.to_string(),
                "p.new: absent → 1".to_string(),
            ]
        );
    }

    #[test]
    fn a_string_keeps_its_quotes_so_a_number_is_not_mistaken_for_one() {
        assert_eq!(show(&json!("1")), r#""1""#);
        assert_eq!(show(&json!(1)), "1");
    }

    #[test]
    fn the_write_is_two_space_indented_sorted_and_newline_terminated() {
        assert_eq!(
            render_json(&obj(json!({"b": 1, "a": {"d": 2}}))),
            "{\n  \"a\": {\n    \"d\": 2\n  },\n  \"b\": 1\n}\n"
        );
    }
}
