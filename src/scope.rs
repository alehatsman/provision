//! Variable scopes and precedence. Spec §3.3.
//!
//! Lowest to highest: facts, then `vars`/`vars_file` in file order, then the
//! command line. `props.*` is a separate namespace that cannot collide, and
//! `env.*` is the read-only process environment.

use minijinja::Value;
use std::collections::BTreeMap;
use std::rc::Rc;

pub type Map = BTreeMap<String, Value>;

/// Shared across every scope in a run: the layers no plan file can change.
pub struct Globals {
    pub facts: Map,
    pub cli: Map,
    pub env: Value,
}

impl Globals {
    pub fn new(facts: &crate::facts::Facts, cli: Map) -> Globals {
        let env: Map = std::env::vars().map(|(k, v)| (k, Value::from(v))).collect();
        Globals {
            facts: facts.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            cli,
            env: Value::from(env),
        }
    }
}

/// One plan file's scope. `import` shares one of these; `use` makes a child.
pub struct Scope {
    globals: Rc<Globals>,
    /// A parent's variables, flattened at the moment the child was made.
    /// Read-only by construction: the child never writes here.
    inherited: Map,
    /// This scope's own `vars`, `vars_file` and `register` results.
    own: Map,
    /// Only inside a component. Reachable as `props.<name>`.
    props: Option<Map>,
}

impl Scope {
    pub fn root(globals: Rc<Globals>) -> Scope {
        Scope { globals, inherited: Map::new(), own: Map::new(), props: None }
    }

    /// A component's scope: the parent's variables, read-only, plus props.
    pub fn child_with_props(&self, props: Map) -> Scope {
        let mut inherited = self.inherited.clone();
        inherited.extend(self.own.clone());
        Scope {
            globals: Rc::clone(&self.globals),
            inherited,
            own: Map::new(),
            props: Some(props),
        }
    }

    pub fn set(&mut self, key: impl Into<String>, value: Value) {
        self.own.insert(key.into(), value);
    }

    /// Precedence, resolved for one name. Expansion renders through `ctx()`;
    /// this is what the precedence tests assert against.
    #[cfg(test)]
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.globals
            .cli
            .get(key)
            .or_else(|| self.own.get(key))
            .or_else(|| self.inherited.get(key))
            .or_else(|| self.globals.facts.get(key))
    }

    /// The rendering context. Built fresh per render; a plan holds tens of
    /// variables, so merging costs nothing worth optimising.
    pub fn ctx(&self) -> Value {
        let mut m = self.globals.facts.clone();
        m.extend(self.inherited.clone());
        m.extend(self.own.clone());
        m.extend(self.globals.cli.clone());
        m.insert("env".to_string(), self.globals.env.clone());
        if let Some(p) = &self.props {
            m.insert("props".to_string(), Value::from(p.clone()));
        }
        Value::from(m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::Facts;

    fn globals(cli: &[(&str, &str)]) -> Rc<Globals> {
        let cli = cli.iter().map(|(k, v)| (k.to_string(), Value::from(*v))).collect();
        Rc::new(Globals::new(&Facts::detect(), cli))
    }

    #[test]
    fn vars_beat_facts_and_the_cli_beats_vars() {
        let mut s = Scope::root(globals(&[]));
        assert_eq!(s.get("os").unwrap().to_string(), std::env::consts::OS.replace("macos", "darwin"));
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

        let mut child = parent.child_with_props(Map::new());
        assert_eq!(child.get("shared").unwrap().to_string(), "p");
        child.set("shared", Value::from("c"));
        child.set("only_child", Value::from(1));

        assert_eq!(child.get("shared").unwrap().to_string(), "c");
        assert_eq!(parent.get("shared").unwrap().to_string(), "p");
        assert!(parent.get("only_child").is_none());
    }

    #[test]
    fn props_are_their_own_namespace() {
        let parent = Scope::root(globals(&[]));
        let mut props = Map::new();
        props.insert("variant".to_string(), Value::from("dark"));
        let child = parent.child_with_props(props);
        let ctx = child.ctx();
        assert_eq!(ctx.get_attr("props").unwrap().get_attr("variant").unwrap().to_string(), "dark");
        // No collision with a same-named variable.
        assert!(child.get("variant").is_none());
    }
}
