//! File resolution and component parsing. Spec §3.1, §3.2.
//!
//! "Files are resolved relative to the file that names them, never the cwd."

use crate::config::model::{self, Step};
use crate::error::{Diag, Result};
use crate::template::expanduser;
use crate::yaml::{Doc, N};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Caches parsed files so a plan that imports the same component twice parses
/// it once, and so `Doc`s are leaked once each.
#[derive(Default)]
pub(crate) struct Loader {
    docs: HashMap<PathBuf, &'static Doc>,
}

impl Loader {
    pub(crate) fn new() -> Loader {
        Loader::default()
    }

    pub(crate) fn load(&mut self, path: &Path) -> Result<&'static Doc> {
        if let Some(d) = self.docs.get(path) {
            return Ok(d);
        }
        let doc = Doc::load(path)?;
        self.docs.insert(path.to_path_buf(), doc);
        Ok(doc)
    }
}

/// Resolve a path named inside `from`, relative to that file's directory.
/// `~` expands first, and an absolute path is taken as given.
pub(crate) fn resolve(from: &Path, rel: &str) -> PathBuf {
    let expanded = expanduser(rel);
    let p = Path::new(&expanded);
    if p.is_absolute() {
        return p.to_path_buf();
    }
    let dir = from.parent().unwrap_or(Path::new("."));
    normalize(&dir.join(p))
}

/// Collapse `.` and `..` textually. Not `canonicalize`: that fails on paths
/// that do not exist yet, and the caller wants to report *that* itself.
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

// ── components ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PropType {
    String,
    Bool,
    Int,
    List,
}

impl PropType {
    fn parse(at: N<'_>) -> Result<PropType> {
        Ok(match at.as_str()? {
            "string" => PropType::String,
            "bool" => PropType::Bool,
            "int" => PropType::Int,
            "list" => PropType::List,
            other => {
                return Err(at
                    .err(format!("`{other}` is not a prop type"))
                    .with_note("one of: string, bool, int, list"));
            }
        })
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            PropType::String => "string",
            PropType::Bool => "bool",
            PropType::Int => "int",
            PropType::List => "list",
        }
    }

    /// What a command-line value of this type looks like. Named in the
    /// error when `--prop` cannot read one (§8).
    pub(crate) fn example(self) -> &'static str {
        match self {
            PropType::String => "any text",
            PropType::Bool => "true or false",
            PropType::Int => "a whole number, like 3",
            PropType::List => "a flow sequence, like [a, b]",
        }
    }

    /// Does a rendered value satisfy this type? Spec §3.2: checked *after*
    /// rendering, because `variant: "{{ v }}"` is a template until then.
    pub(crate) fn accepts(self, v: &minijinja::Value) -> bool {
        use minijinja::value::ValueKind as Kind;
        match self {
            PropType::String => matches!(v.kind(), Kind::String),
            PropType::Bool => matches!(v.kind(), Kind::Bool),
            PropType::Int => matches!(v.kind(), Kind::Number) && v.as_i64().is_some(),
            PropType::List => matches!(v.kind(), Kind::Seq),
        }
    }
}

pub(crate) struct PropSchema {
    pub name: String,
    pub ty: PropType,
    pub required: bool,
    pub default: Option<N<'static>>,
    /// Spec §3.2. Accepted since props existed and read by nothing until
    /// `list <component.yml>` (§8) — which is that command's whole point: a
    /// declared interface nobody could print is one nobody outside the file
    /// could use.
    pub description: Option<String>,
    pub at: N<'static>,
}

pub(crate) struct Component {
    pub path: PathBuf,
    pub props: Vec<PropSchema>,
    pub steps: Vec<Step<'static>>,
    /// D17, spec §3.2: optional, and read only by `run <dir>/`.
    pub description: Option<String>,
}

impl Component {
    pub(crate) fn prop(&self, name: &str) -> Option<&PropSchema> {
        self.props.iter().find(|p| p.name == name)
    }
}

/// Parse a component file: a mapping with `props` and `steps`, not a list.
pub(crate) fn parse_component(doc: &'static Doc) -> Result<Component> {
    let root = doc.node();
    #[expect(clippy::map_err_ignore, reason = "the replacement diagnostic restates the cause")]
    root.as_map().map_err(|_| {
        root.err(format!("a component is a mapping, found {}", root.kind())).with_note(
            "a component has `steps:` and optional `props:`; a plan is a bare list and is entered with `import:`",
        )
    })?;
    // `description` is D17's one format change: read only by
    // `provision run <dir>/` when listing, ignored everywhere else.
    root.deny_unknown_keys(&["props", "steps", "description"], "a component")?;

    #[expect(
        clippy::map_err_ignore,
        reason = "the replacement diagnostic restates the cause"
    )]
    let steps_at = root.require("steps").map_err(|_| {
        root.err("component has no `steps:`")
            .with_note("a component is `props:` (optional) plus `steps:`")
    })?;
    let (steps, step_errors) = model::parse_steps(steps_at)?;
    if let Some(first) = step_errors.into_iter().next() {
        return Err(first);
    }

    let mut props = Vec::new();
    if let Some(props_at) = root.get("props") {
        for (name, name_at) in props_at.keys()? {
            let schema = props_at.require(name)?;
            schema.deny_unknown_keys(
                &["type", "default", "required", "description"],
                &format!("prop `{name}`"),
            )?;
            #[expect(
                clippy::map_err_ignore,
                reason = "the replacement diagnostic restates the cause"
            )]
            let ty = PropType::parse(schema.require("type").map_err(|_| {
                schema
                    .err(format!("prop `{name}` has no `type`"))
                    .with_note("one of: string, bool, int, list")
            })?)?;
            let required = match schema.get("required") {
                Some(r) => r.as_bool()?,
                None => false,
            };
            let default = schema.get("default");
            if required && default.is_some() {
                return Err(schema
                    .err(format!("prop `{name}` is both required and has a default"))
                    .with_note("a default makes it optional; drop one"));
            }
            let description = match schema.get("description") {
                Some(n) => Some(n.as_scalar_string()?),
                None => None,
            };
            props.push(PropSchema {
                name: name.to_string(),
                ty,
                required,
                default,
                description,
                at: name_at,
            });
        }
    }

    let description = match root.get("description") {
        Some(n) => Some(n.as_scalar_string()?),
        None => None,
    };

    Ok(Component {
        path: doc.path.clone(),
        props,
        steps,
        description,
    })
}

/// A missing file, reported against the node that named it.
pub(crate) fn missing(at: N<'_>, path: &Path, what: &str) -> Diag {
    at.err(format!("{what} not found: {}", path.display()))
        .with_note("paths are resolved relative to the file that names them, not the cwd")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_resolve_against_the_naming_file() {
        let from = Path::new("/p/machines/x1/index.yml");
        assert_eq!(
            resolve(from, "./vars.yml"),
            PathBuf::from("/p/machines/x1/vars.yml")
        );
        assert_eq!(
            resolve(from, "../../components/zsh/index.yml"),
            PathBuf::from("/p/components/zsh/index.yml")
        );
        assert_eq!(resolve(from, "/etc/x.yml"), PathBuf::from("/etc/x.yml"));
    }

    #[test]
    fn tilde_expands_before_resolution() {
        let home = home::home_dir().unwrap();
        assert_eq!(
            resolve(Path::new("/p/a.yml"), "~/x.yml"),
            home.join("x.yml")
        );
    }

    #[test]
    fn prop_types_check_rendered_values() {
        use minijinja::Value;
        assert!(PropType::String.accepts(&Value::from("a")));
        assert!(!PropType::String.accepts(&Value::from(1)));
        assert!(PropType::Bool.accepts(&Value::from(true)));
        assert!(!PropType::Bool.accepts(&Value::from("true")));
        assert!(PropType::Int.accepts(&Value::from(3)));
        assert!(PropType::List.accepts(&Value::from(vec!["a"])));
    }
}
