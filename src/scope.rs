//! Variable scopes and precedence. Spec §3.3.
//!
//! Lowest to highest: facts, then `vars`/`vars_file` in file order, then the
//! command line. `props.*` is a separate namespace that cannot collide, and
//! `env.*` is the read-only process environment.

use minijinja::Value;
use std::collections::BTreeMap;
use std::rc::Rc;

pub(crate) type Map = BTreeMap<String, Value>;

/// Shared across every scope in a run: the layers no plan file can change.
pub(crate) struct Globals {
    pub facts: Map,
    pub cli: Map,
    pub env: Value,
}

impl Globals {
    pub(crate) fn new(facts: &crate::facts::Facts, cli: Map) -> Globals {
        let env: Map = std::env::vars().map(|(k, v)| (k, Value::from(v))).collect();
        Globals {
            facts: facts.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            cli,
            env: Value::from(env),
        }
    }
}

/// One plan file's scope. `import` shares one of these; `use` makes a child.
pub(crate) struct Scope {
    globals: Rc<Globals>,
    /// A parent's variables, flattened at the moment the child was made.
    /// Read-only by construction: the child never writes here.
    inherited: Map,
    /// This scope's own `vars`, `vars_file` and `register` results.
    own: Map,
    /// Only inside a component. Reachable as `props.<name>`.
    props: Option<Map>,
    /// Spec §3.2: the component file's own directory, absolute. A shared
    /// component that ships scripts has no other way to name them — a path
    /// inside a shell string is not a path provision resolves.
    component_dir: Option<String>,
}

impl Scope {
    pub(crate) fn root(globals: Rc<Globals>) -> Scope {
        Scope {
            globals,
            inherited: Map::new(),
            own: Map::new(),
            props: None,
            component_dir: None,
        }
    }

    /// A component's scope: the parent's variables, read-only, plus props and
    /// the component's own directory (§3.2). `dir` is the component file's
    /// parent, made absolute — a relative one would be read against whatever
    /// the process cwd happens to be, which under `run` is deliberately not
    /// the component's directory.
    pub(crate) fn child_with_props(&self, props: Map, dir: &std::path::Path) -> Scope {
        let mut inherited = self.inherited.clone();
        inherited.extend(self.own.clone());
        let dir = std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf());
        Scope {
            globals: Rc::clone(&self.globals),
            inherited,
            own: Map::new(),
            props: Some(props),
            component_dir: Some(dir.display().to_string()),
        }
    }

    pub(crate) fn set(&mut self, key: impl Into<String>, value: Value) {
        self.own.insert(key.into(), value);
    }

    /// Precedence, resolved for one name. Expansion renders through `ctx()`;
    /// this is what the precedence tests assert against.
    #[cfg(test)]
    pub(crate) fn get(&self, key: &str) -> Option<&Value> {
        self.globals
            .cli
            .get(key)
            .or_else(|| self.own.get(key))
            .or_else(|| self.inherited.get(key))
            .or_else(|| self.globals.facts.get(key))
    }

    /// The rendering context. Built fresh per render; a plan holds tens of
    /// variables, so merging costs nothing worth optimising.
    pub(crate) fn ctx(&self) -> Value {
        Value::from(self.ctx_map())
    }

    /// The same context before it is sealed into a `Value`. The runner's
    /// judge needs to add `result` to it, and a `Value` map cannot be extended.
    pub(crate) fn ctx_map(&self) -> Map {
        let mut m = self.globals.facts.clone();
        m.extend(self.inherited.clone());
        m.extend(self.own.clone());
        m.extend(self.globals.cli.clone());
        m.insert("env".to_string(), self.globals.env.clone());
        if let Some(p) = &self.props {
            m.insert("props".to_string(), Value::from(p.clone()));
        }
        if let Some(d) = &self.component_dir {
            m.insert("component_dir".to_string(), Value::from(d.clone()));
        }
        m
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::Facts;
    use std::path::Path;

    fn globals(cli: &[(&str, &str)]) -> Rc<Globals> {
        let cli = cli
            .iter()
            .map(|(k, v)| (k.to_string(), Value::from(*v)))
            .collect();
        Rc::new(Globals::new(&Facts::detect(), cli))
    }

    #[test]
    fn vars_beat_facts_and_the_cli_beats_vars() {
        let mut s = Scope::root(globals(&[]));
        assert_eq!(
            s.get("os").unwrap().to_string(),
            std::env::consts::OS.replace("macos", "darwin")
        );
        s.set("os", Value::from("overridden"));
        assert_eq!(s.get("os").unwrap().to_string(), "overridden");

        let mut s = Scope::root(globals(&[("os", "from-cli")]));
        s.set("os", Value::from("from-vars"));
        assert_eq!(s.get("os").unwrap().to_string(), "from-cli");
    }

    #[test]
    fn later_vars_win() {
        let mut s = Scope::root(globals(&[]));
        s.set("k", Value::from(1));
        s.set("k", Value::from(2));
        assert_eq!(s.get("k").unwrap().to_string(), "2");
    }

    #[test]
    fn a_child_sees_the_parent_but_does_not_leak_back() {
        let mut parent = Scope::root(globals(&[]));
        parent.set("shared", Value::from("p"));

        let mut child = parent.child_with_props(Map::new(), Path::new("/c"));
        assert_eq!(child.get("shared").unwrap().to_string(), "p");
        child.set("shared", Value::from("c"));
        child.set("only_child", Value::from(1));

        assert_eq!(child.get("shared").unwrap().to_string(), "c");
        assert_eq!(parent.get("shared").unwrap().to_string(), "p");
        assert!(parent.get("only_child").is_none());
    }

    // Spec §3.2: a shared component that ships scripts reaches them through
    // this and nothing else, so it has to be absolute and it has to be the
    // component's own directory, not the caller's.
    #[test]
    fn a_component_scope_carries_its_own_directory() {
        let parent = Scope::root(globals(&[]));
        let child = parent.child_with_props(Map::new(), Path::new("/tools/rq"));
        assert_eq!(
            child.ctx().get_attr("component_dir").unwrap().to_string(),
            "/tools/rq"
        );
        // A plan's own scope has none: `import` shares the caller's scope and
        // is not a component.
        assert!(
            parent
                .ctx()
                .get_attr("component_dir")
                .unwrap()
                .is_undefined()
        );
    }

    #[test]
    fn props_are_their_own_namespace() {
        let parent = Scope::root(globals(&[]));
        let mut props = Map::new();
        props.insert("variant".to_string(), Value::from("dark"));
        let child = parent.child_with_props(props, Path::new("/c"));
        let ctx = child.ctx();
        assert_eq!(
            ctx.get_attr("props")
                .unwrap()
                .get_attr("variant")
                .unwrap()
                .to_string(),
            "dark"
        );
        // No collision with a same-named variable.
        assert!(child.get("variant").is_none());
    }
}
