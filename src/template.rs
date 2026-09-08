//! One template engine for both string fields and conditions. Spec §3.4, §3.5.
//!
//! Strict undefined: reading a variable that was never set is an error with
//! the variable's name, not an empty string (D6).

use crate::error::{Diag, Result};
use crate::yaml::N;
use minijinja::value::{Value, ValueKind as Kind};
use minijinja::{Environment, UndefinedBehavior};
use std::error::Error as _;
use std::fmt::Write as _;
use std::path::Path;

pub(crate) struct Engine {
    env: Environment<'static>,
}

impl Engine {
    pub(crate) fn new() -> Engine {
        let mut env = Environment::new();
        env.set_undefined_behavior(UndefinedBehavior::Strict);
        env.set_keep_trailing_newline(true);
        env.add_filter("expanduser", f_expanduser);
        env.add_filter("basename", f_basename);
        env.add_filter("dirname", f_dirname);
        env.add_filter("quote", f_quote);
        env.add_filter("to_json", f_to_json);
        env.add_filter("to_yaml", f_to_yaml);
        env.add_test("exists", t_exists);
        Engine { env }
    }

    /// Render a string field. Errors are anchored by the caller.
    pub(crate) fn render(
        &self,
        src: &str,
        ctx: &Value,
    ) -> std::result::Result<String, minijinja::Error> {
        self.env.render_str(src, ctx)
    }

    /// Render a *field value*, preserving the type when the whole field is one
    /// expression.
    ///
    /// `names: "{{ apps }}"` must yield the list `apps` holds, not its textual
    /// rendering. `path: "{{ home }}/.zshrc"` is a string. The rule is exactly
    /// "the field is one `{{ … }}` and nothing else".
    pub(crate) fn render_field(
        &self,
        src: &str,
        ctx: &Value,
    ) -> std::result::Result<Value, minijinja::Error> {
        match sole_expression(src) {
            Some(expr) => self.eval(expr, ctx),
            None => self.render(src, ctx).map(Value::from),
        }
    }

    pub(crate) fn eval(
        &self,
        expr: &str,
        ctx: &Value,
    ) -> std::result::Result<Value, minijinja::Error> {
        self.env.compile_expression(expr)?.eval(ctx)
    }

    /// Evaluate a condition. Spec §3.5: a non-boolean result is an error, not
    /// truthy — `when: some_string` is a bug, not a green light.
    pub(crate) fn eval_bool(
        &self,
        expr: &str,
        ctx: &Value,
        at: &dyn Fn(String) -> Diag,
    ) -> Result<bool> {
        let v = self.eval(expr, ctx).map_err(|e| at(describe(&e)))?;
        match v.kind() {
            Kind::Bool => Ok(v.is_true()),
            _ => Err(at(format!(
                "condition `{expr}` evaluated to {} ({}), not true or false",
                v,
                kind_name(&v)
            ))),
        }
    }

    /// The variable names an expression reads. Used to tell whether a `when`
    /// depends on a `register` that has not run yet.
    /// Top-level names, so `r.changed` reports `r`.
    pub(crate) fn undeclared(&self, expr: &str) -> Option<std::collections::HashSet<String>> {
        self.env
            .compile_expression(expr)
            .ok()
            .map(|e| e.undeclared_variables(false))
    }

    /// Render a spanned node into a value: strings through the field rule of
    /// §3.4, sequences and mappings element by element, everything else as
    /// written. Errors are anchored at the node that failed, not the root.
    pub(crate) fn render_node(&self, at: N<'_>, ctx: &Value) -> Result<Value> {
        match at.as_str() {
            Ok(s) => self
                .render_field(s, ctx)
                .map_err(|e| at.err(self.describe_with(s, ctx, &e))),
            // Non-strings are taken as written, but their strings are rendered.
            Err(_) => match at.as_seq() {
                Ok(items) => {
                    let vs: Result<Vec<Value>> = items
                        .into_iter()
                        .map(|i| self.render_node(i, ctx))
                        .collect();
                    Ok(Value::from(vs?))
                }
                Err(_) => match at.as_map() {
                    Ok(pairs) => {
                        let mut m = std::collections::BTreeMap::new();
                        for (k, v) in pairs {
                            m.insert(k.as_scalar_string()?, self.render_node(v, ctx)?);
                        }
                        Ok(Value::from(m))
                    }
                    Err(_) => at.to_value(),
                },
            },
        }
    }

    /// Check template syntax without a context. Used by `validate`.
    pub(crate) fn check_syntax(&self, src: &str, at: &dyn Fn(String) -> Diag) -> Result<()> {
        match sole_expression(src) {
            Some(expr) => self.env.compile_expression(expr).map(|_| ()),
            None => self.env.template_from_str(src).map(|_| ()),
        }
        .map_err(|e| at(describe(&e)))
    }
}

/// `"{{ x }}"` → `Some("x")`. `"a{{ x }}"`, `"{{ x }}{{ y }}"` → `None`.
fn sole_expression(src: &str) -> Option<&str> {
    let s = src.trim();
    let inner = s.strip_prefix("{{")?.strip_suffix("}}")?;
    if inner.contains("}}") || inner.contains("{{") || inner.contains("{%") {
        return None;
    }
    Some(inner.trim())
}

impl Engine {
    /// Spec §3.4 wants the *name* of the undefined variable, which minijinja's
    /// message omits. Recover it by asking the template what it reads and
    /// checking each name against the context.
    pub(crate) fn describe_with(&self, src: &str, ctx: &Value, e: &minijinja::Error) -> String {
        let base = describe(e);
        if e.kind() != minijinja::ErrorKind::UndefinedError {
            return base;
        }
        let referenced = match sole_expression(src) {
            Some(expr) => self
                .env
                .compile_expression(expr)
                .map(|c| c.undeclared_variables(false)),
            None => self
                .env
                .template_from_str(src)
                .map(|t| t.undeclared_variables(false)),
        };
        let Ok(names) = referenced else { return base };
        let mut missing: Vec<String> = names
            .into_iter()
            .filter(|n| ctx.get_attr(n).map_or(true, |v| v.is_undefined()))
            .collect();
        missing.sort();
        match missing.as_slice() {
            [] => base,
            [one] => format!("undefined variable `{one}`"),
            _ => format!("undefined variables: {}", missing.join(", ")),
        }
    }
}

/// minijinja nests the useful part in the source of the error; surface both.
pub(crate) fn describe(e: &minijinja::Error) -> String {
    let mut msg = e.to_string();
    let mut src = e.source();
    while let Some(s) = src {
        write!(msg, ": {s}").expect("writing to a String cannot fail");
        src = s.source();
    }
    msg
}

fn kind_name(v: &Value) -> &'static str {
    match v.kind() {
        Kind::Undefined => "undefined",
        Kind::None => "none",
        Kind::Bool => "a boolean",
        Kind::Number => "a number",
        Kind::String => "a string",
        Kind::Seq => "a list",
        Kind::Map => "a mapping",
        _ => "a value",
    }
}

// ── filters ───────────────────────────────────────────────────────────────

fn f_expanduser(s: &str) -> String {
    expanduser(s)
}

/// `~` and `~/…` only. `~other` is left alone: the tool has no business
/// guessing another account's home (D8 — root or the current user).
pub(crate) fn expanduser(s: &str) -> String {
    let Some(rest) = s.strip_prefix('~') else {
        return s.to_string();
    };
    if !(rest.is_empty() || rest.starts_with('/') || rest.starts_with('\\')) {
        return s.to_string();
    }
    match home::home_dir() {
        Some(h) => format!("{}{}", h.display(), rest),
        None => s.to_string(),
    }
}

fn f_basename(s: &str) -> String {
    Path::new(s)
        .file_name()
        .map_or_else(|| s.to_string(), |n| n.to_string_lossy().into_owned())
}

fn f_dirname(s: &str) -> String {
    Path::new(s)
        .parent()
        .map(|n| n.display().to_string())
        .unwrap_or_default()
}

/// POSIX single-quote shell escaping. Safe for every byte.
fn f_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

fn f_to_json(v: &Value) -> std::result::Result<String, minijinja::Error> {
    serde_json::to_string(v)
        .map_err(|e| minijinja::Error::new(minijinja::ErrorKind::InvalidOperation, e.to_string()))
}

fn f_to_yaml(v: &Value) -> String {
    let mut out = String::new();
    emit_yaml(v, 0, &mut out);
    out
}

fn t_exists(s: &str) -> bool {
    Path::new(&expanduser(s)).exists()
}

// ── a small YAML emitter ─────────────────────────────────────────────────
// Only the shapes a plan can hold: scalars, sequences, mappings.

fn emit_yaml(v: &Value, indent: usize, out: &mut String) {
    let pad = "  ".repeat(indent);
    match v.kind() {
        Kind::Seq => {
            let items: Vec<Value> = v.try_iter().map(Iterator::collect).unwrap_or_default();
            if items.is_empty() {
                out.push_str("[]\n");
                return;
            }
            if indent > 0 {
                out.push('\n');
            }
            for item in items {
                out.push_str(&pad);
                out.push_str("- ");
                emit_nested(&item, indent + 1, out);
            }
        }
        Kind::Map => {
            let keys: Vec<Value> = v.try_iter().map(Iterator::collect).unwrap_or_default();
            if keys.is_empty() {
                out.push_str("{}\n");
                return;
            }
            if indent > 0 {
                out.push('\n');
            }
            for k in keys {
                let val = v.get_item(&k).unwrap_or_default();
                out.push_str(&pad);
                out.push_str(&scalar_yaml(&k));
                out.push_str(": ");
                emit_nested(&val, indent + 1, out);
            }
        }
        _ => {
            out.push_str(&scalar_yaml(v));
            out.push('\n');
        }
    }
}

fn emit_nested(v: &Value, indent: usize, out: &mut String) {
    match v.kind() {
        Kind::Seq | Kind::Map => {
            // emit_yaml opens a nested block with the newline that separates it
            // from the `key:` already written. Keep it.
            emit_yaml(v, indent, out);
        }
        _ => {
            out.push_str(&scalar_yaml(v));
            out.push('\n');
        }
    }
}

fn scalar_yaml(v: &Value) -> String {
    match v.kind() {
        Kind::None | Kind::Undefined => "null".to_string(),
        Kind::Bool | Kind::Number => v.to_string(),
        _ => {
            let s = v.to_string();
            // Quote anything that would not round-trip as a plain scalar.
            let plain = !s.is_empty()
                && !s.starts_with(' ')
                && !s.ends_with(' ')
                && !s.contains(['\n', '"', '\'', ':', '#', '{', '}', '[', ']', ',', '&', '*'])
                && !matches!(
                    s.as_str(),
                    "true" | "false" | "null" | "yes" | "no" | "on" | "off" | "~"
                )
                && s.parse::<f64>().is_err();
            if plain { s } else { format!("{s:?}") }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn ctx(pairs: &[(&str, Value)]) -> Value {
        Value::from(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect::<BTreeMap<_, _>>(),
        )
    }

    #[test]
    fn undefined_is_an_error_naming_the_variable() {
        let e = Engine::new();
        let c = ctx(&[]);
        let err = e.render("{{ nope }}", &c).unwrap_err();
        assert_eq!(
            e.describe_with("{{ nope }}", &c, &err),
            "undefined variable `nope`"
        );
    }

    #[test]
    fn a_sole_expression_keeps_its_type() {
        let e = Engine::new();
        let c = ctx(&[("apps", Value::from(vec!["git", "curl"]))]);
        let v = e.render_field("{{ apps }}", &c).unwrap();
        assert_eq!(v.kind(), Kind::Seq);
        assert_eq!(v.len(), Some(2));
    }

    #[test]
    fn an_interpolated_field_is_a_string() {
        let e = Engine::new();
        let c = ctx(&[("home", Value::from("/home/a"))]);
        let v = e.render_field("{{ home }}/.zshrc", &c).unwrap();
        assert_eq!(v.kind(), Kind::String);
        assert_eq!(v.to_string(), "/home/a/.zshrc");
    }

    #[test]
    fn two_expressions_are_a_string() {
        let e = Engine::new();
        let c = ctx(&[("a", Value::from(1)), ("b", Value::from(2))]);
        assert_eq!(
            e.render_field("{{ a }}{{ b }}", &c).unwrap().to_string(),
            "12"
        );
    }

    #[test]
    fn a_non_boolean_condition_is_an_error() {
        let e = Engine::new();
        let at = |m: String| Diag::new("t.yml", 1, 1, m);
        let c = ctx(&[("s", Value::from("yes"))]);
        let err = e.eval_bool("s", &c, &at).unwrap_err();
        assert!(err.msg.contains("not true or false"), "{}", err.msg);
        assert!(e.eval_bool("s == 'yes'", &c, &at).unwrap());
    }

    #[test]
    fn filters() {
        let e = Engine::new();
        let c = ctx(&[("p", Value::from("/a/b/c.txt"))]);
        assert_eq!(e.render("{{ p | basename }}", &c).unwrap(), "c.txt");
        assert_eq!(e.render("{{ p | dirname }}", &c).unwrap(), "/a/b");
        assert_eq!(
            e.render("{{ \"it's\" | quote }}", &c).unwrap(),
            r"'it'\''s'"
        );
        assert_eq!(e.render("{{ [1,2] | to_json }}", &c).unwrap(), "[1,2]");
    }

    #[test]
    fn expanduser_leaves_other_accounts_alone() {
        assert_eq!(expanduser("~root/x"), "~root/x");
        assert!(expanduser("~/x").ends_with("/x"));
        assert!(!expanduser("~/x").starts_with('~'));
    }

    #[test]
    fn to_yaml_round_trips_through_the_parser() {
        use saphyr::LoadableYamlNode;

        let e = Engine::new();
        let mut m = BTreeMap::new();
        m.insert("name".to_string(), Value::from("zsh"));
        m.insert("names".to_string(), Value::from(vec!["git", "curl"]));
        m.insert("on".to_string(), Value::from(true));
        let out = e
            .render("{{ m | to_yaml }}", &ctx(&[("m", Value::from(m))]))
            .unwrap();
        let parsed = saphyr::Yaml::load_from_str(&out).unwrap();
        assert_eq!(parsed.len(), 1, "emitted YAML did not parse: {out}");
    }

    #[test]
    fn raw_blocks_survive() {
        let e = Engine::new();
        let out = e
            .render("{% raw %}${{ x }}{% endraw %}", &ctx(&[]))
            .unwrap();
        assert_eq!(out, "${{ x }}");
    }

    #[test]
    fn an_undefined_variable_is_named() {
        let e = Engine::new();
        let c = ctx(&[("known", Value::from(1))]);
        let err = e.render("{{ known }}{{ palette.bg }}", &c).unwrap_err();
        assert_eq!(
            e.describe_with("{{ known }}{{ palette.bg }}", &c, &err),
            "undefined variable `palette`"
        );
    }
}
