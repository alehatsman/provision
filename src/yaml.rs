//! A spanned YAML layer.
//!
//! Every node — map keys included — carries its `line:col`, which is what
//! lets validation errors point at the exact key that is wrong. Accessors
//! return `Diag` directly so callers never have to reconstruct a position.

use crate::error::{Diag, Result};
use saphyr::{LoadableYamlNode, MarkedYaml, Scalar, YamlData};
use std::path::{Path, PathBuf};

/// A parsed plan file.
///
/// Both the source text and the `Doc` itself are leaked. A plan file is a few
/// kilobytes, is loaded once, and its spans have to outlive every later phase
/// that might report an error against them — which is all of them. Leaking
/// buys `'static` nodes and removes a lifetime from every downstream type,
/// for memory the process returns on exit anyway.
pub struct Doc {
    pub path: PathBuf,
    pub root: MarkedYaml<'static>,
}

impl Doc {
    pub fn load(path: &Path) -> Result<&'static Doc> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| Diag::file_level(path, format!("cannot read: {e}")))?;
        let text: &'static str = Box::leak(text.into_boxed_str());
        let mut docs = MarkedYaml::load_from_str(text).map_err(|e| {
            let m = e.marker();
            Diag::new(path, m.line(), m.col() + 1, format!("invalid YAML: {e}"))
        })?;
        match docs.len() {
            0 => Err(Diag::file_level(path, "file is empty")),
            1 => Ok(Box::leak(Box::new(Doc {
                path: path.to_path_buf(),
                root: docs.remove(0),
            }))),
            n => Err(Diag::file_level(
                path,
                format!("{n} YAML documents in one file; a plan is one document"),
            )),
        }
    }

    pub fn node(&'static self) -> N<'static> {
        N { node: &self.root, file: &self.path }
    }
}

/// A node plus the file it came from. Copy, so it passes around freely.
#[derive(Clone, Copy)]
pub struct N<'a> {
    pub node: &'a MarkedYaml<'static>,
    pub file: &'a Path,
}

impl<'a> N<'a> {
    fn wrap(&self, node: &'a MarkedYaml<'static>) -> N<'a> {
        N { node, file: self.file }
    }

    pub fn line(&self) -> usize {
        self.node.span.start.line()
    }

    pub fn col(&self) -> usize {
        self.node.span.start.col() + 1
    }

    /// Build a diagnostic anchored at this node.
    pub fn err(&self, msg: impl Into<String>) -> Diag {
        Diag::new(self.file, self.line(), self.col(), msg)
    }

    pub fn kind(&self) -> &'static str {
        match &self.node.data {
            YamlData::Value(Scalar::String(_)) => "a string",
            YamlData::Value(Scalar::Integer(_)) => "an integer",
            YamlData::Value(Scalar::FloatingPoint(_)) => "a float",
            YamlData::Value(Scalar::Boolean(_)) => "a boolean",
            YamlData::Value(Scalar::Null) => "null",
            YamlData::Sequence(_) => "a list",
            YamlData::Mapping(_) => "a mapping",
            _ => "an unsupported node",
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(&self.node.data, YamlData::Value(Scalar::Null))
    }

    pub fn as_str(&self) -> Result<&'a str> {
        match &self.node.data {
            YamlData::Value(Scalar::String(s)) => Ok(s),
            _ => Err(self.err(format!("expected a string, found {}", self.kind()))),
        }
    }

    pub fn as_bool(&self) -> Result<bool> {
        match &self.node.data {
            YamlData::Value(Scalar::Boolean(b)) => Ok(*b),
            _ => Err(self.err(format!("expected true or false, found {}", self.kind()))),
        }
    }

    pub fn as_seq(&self) -> Result<Vec<N<'a>>> {
        match &self.node.data {
            YamlData::Sequence(items) => Ok(items.iter().map(|i| self.wrap(i)).collect()),
            _ => Err(self.err(format!("expected a list, found {}", self.kind()))),
        }
    }

    pub fn as_map(&self) -> Result<Vec<(N<'a>, N<'a>)>> {
        match &self.node.data {
            YamlData::Mapping(m) => {
                Ok(m.iter().map(|(k, v)| (self.wrap(k), self.wrap(v))).collect())
            }
            _ => Err(self.err(format!("expected a mapping, found {}", self.kind()))),
        }
    }

    /// The keys of a mapping, as strings, in document order. A non-string key
    /// is an error: plan files have no use for them.
    pub fn keys(&self) -> Result<Vec<(&'a str, N<'a>)>> {
        self.as_map()?
            .into_iter()
            .map(|(k, _)| k.as_str().map(|s| (s, k)))
            .collect()
    }

    /// Look up one key. `None` when absent.
    pub fn get(&self, key: &str) -> Option<N<'a>> {
        let YamlData::Mapping(m) = &self.node.data else { return None };
        m.iter()
            .find(|(k, _)| matches!(&k.data, YamlData::Value(Scalar::String(s)) if s == key))
            .map(|(_, v)| self.wrap(v))
    }

    /// Look up one key, erroring at *this* node when it is missing.
    pub fn require(&self, key: &str) -> Result<N<'a>> {
        self.get(key).ok_or_else(|| self.err(format!("missing required key `{key}`")))
    }

    /// Reject any key not in `allowed`, pointing at the offending key. Spec §3:
    /// "Unknown top-level keys in a step are errors. No silent typos."
    pub fn deny_unknown_keys(&self, allowed: &[&str], ctx: &str) -> Result<()> {
        for (name, at) in self.keys()? {
            if !allowed.contains(&name) {
                let mut d = at.err(format!("unknown key `{name}` in {ctx}"));
                if let Some(near) = nearest(name, allowed) {
                    d = d.with_note(format!("did you mean `{near}`?"));
                } else {
                    d = d.with_note(format!("allowed: {}", allowed.join(", ")));
                }
                return Err(d);
            }
        }
        Ok(())
    }

    /// A scalar as a plain string, for fields where YAML may have typed it:
    /// `mode: 0644` parses as an integer, `enabled: yes` as a bool.
    pub fn as_scalar_string(&self) -> Result<String> {
        match &self.node.data {
            YamlData::Value(Scalar::String(s)) => Ok(s.to_string()),
            YamlData::Value(Scalar::Integer(i)) => Ok(i.to_string()),
            YamlData::Value(Scalar::Boolean(b)) => Ok(b.to_string()),
            YamlData::Value(Scalar::FloatingPoint(f)) => Ok(f.into_inner().to_string()),
            _ => Err(self.err(format!("expected a scalar, found {}", self.kind()))),
        }
    }

    /// A string, or a list of strings. `tags: core` and `tags: [core]` both work.
    pub fn as_str_or_seq(&self) -> Result<Vec<&'a str>> {
        match &self.node.data {
            YamlData::Sequence(_) => self.as_seq()?.iter().map(|n| n.as_str()).collect(),
            _ => Ok(vec![self.as_str()?]),
        }
    }

    /// A single node, or the items of a list. `vars_file: p` and
    /// `vars_file: [p, q]` are both legal (spec §3.1).
    pub fn as_str_or_seq_nodes(&self) -> Result<Vec<N<'a>>> {
        match &self.node.data {
            YamlData::Sequence(_) => self.as_seq(),
            _ => {
                self.as_str()?;
                Ok(vec![*self])
            }
        }
    }

    /// Convert to a template value, preserving YAML types.
    #[allow(clippy::wrong_self_convention)] // N is Copy; taking &self reads better here
    pub fn to_value(&self) -> Result<minijinja::Value> {
        use minijinja::Value as V;
        Ok(match &self.node.data {
            YamlData::Value(Scalar::String(s)) => V::from(s.to_string()),
            YamlData::Value(Scalar::Integer(i)) => V::from(*i),
            YamlData::Value(Scalar::FloatingPoint(f)) => V::from(f.into_inner()),
            YamlData::Value(Scalar::Boolean(b)) => V::from(*b),
            YamlData::Value(Scalar::Null) => V::from(()),
            YamlData::Sequence(_) => {
                let items: Result<Vec<_>> = self.as_seq()?.iter().map(|n| n.to_value()).collect();
                V::from(items?)
            }
            YamlData::Mapping(_) => {
                let mut m = std::collections::BTreeMap::new();
                for (k, v) in self.as_map()? {
                    m.insert(k.as_scalar_string()?, v.to_value()?);
                }
                V::from(m)
            }
            _ => return Err(self.err("unsupported YAML node")),
        })
    }
}

/// Edit distance of 1-2, for "did you mean". Cheap and good enough for typos
/// in a fixed vocabulary of about twenty keys.
fn nearest<'k>(got: &str, allowed: &[&'k str]) -> Option<&'k str> {
    allowed
        .iter()
        .map(|a| (levenshtein(got, a), *a))
        .filter(|(d, _)| *d <= 2)
        .min_by_key(|(d, _)| *d)
        .map(|(_, a)| a)
}

fn levenshtein(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for (i, ca) in a.chars().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != *cb);
            cur[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(text: &str) -> &'static Doc {
        let text: &'static str = Box::leak(text.to_string().into_boxed_str());
        let mut docs = MarkedYaml::load_from_str(text).unwrap();
        Box::leak(Box::new(Doc { path: PathBuf::from("t.yml"), root: docs.remove(0) }))
    }

    #[test]
    fn keys_carry_their_own_position() {
        let d = doc("- name: a\n  shell: b\n");
        let step = d.node().as_seq().unwrap()[0];
        let (_, at) = step.keys().unwrap()[1];
        assert_eq!((at.line(), at.col()), (2, 3));
    }

    #[test]
    fn unknown_key_points_at_the_key_and_suggests() {
        let d = doc("- name: a\n  shel: b\n");
        let step = d.node().as_seq().unwrap()[0];
        let e = step.deny_unknown_keys(&["name", "shell"], "a step").unwrap_err();
        assert_eq!((e.line, e.col), (2, 3));
        assert_eq!(e.note.as_deref(), Some("did you mean `shell`?"));
    }

    #[test]
    fn scalars_keep_their_yaml_type() {
        let d = doc("mode: 0644\nflag: true\nnames: [a, b]\n");
        let n = d.node();
        assert_eq!(n.require("mode").unwrap().as_scalar_string().unwrap(), "644");
        assert!(n.require("flag").unwrap().as_bool().unwrap());
        assert_eq!(n.require("names").unwrap().as_str_or_seq().unwrap(), vec!["a", "b"]);
    }

    #[test]
    fn type_errors_name_what_was_found() {
        let d = doc("- shell: [a]\n");
        let step = d.node().as_seq().unwrap()[0];
        let e = step.require("shell").unwrap().as_str().unwrap_err();
        assert_eq!(e.msg, "expected a string, found a list");
    }
}
