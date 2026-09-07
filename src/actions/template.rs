//! `template` — render a file, or a tree of them, and place the result.
//! Spec §6.4.
//!
//! A template is a `file` with its content computed, so that is exactly what
//! this builds: one `file::Spec` per output, applied in order and reported as
//! one step. Directory mode is the only iteration in the language, and it is
//! not a loop — no `item`, no per-file scope, no user-supplied collection.

use super::{Ctx, Effect, file};
use crate::config::model::Step;
use crate::error::Result;
use crate::template::{Engine, expanduser};
use crate::yaml::N;
use minijinja::Value;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spec {
    /// One entry per output file, already rendered. Each carries the label
    /// its diff is headed by (§6.4).
    outputs: Vec<file::Spec>,
    dest: String,
    /// True when `src` named a directory. Changes what `dest` must be.
    tree: bool,
}

pub fn parse(step: &Step<'_>, engine: &Engine, ctx: &Value, raw: bool) -> Result<Option<Spec>> {
    let body = step.body;
    let render = |src: &str| -> Option<String> {
        if raw { Some(src.to_string()) } else { engine.render(src, ctx).ok() }
    };

    let Some(src_at) = body.get("src") else {
        return Err(body.err("`template` requires `src`"));
    };
    let Some(dest_at) = body.get("dest") else {
        return Err(body.err("`template` requires `dest`"));
    };
    let Some(src_rel) = render(src_at.as_str()?) else { return Ok(None) };
    let Some(dest) = render(dest_at.as_str()?) else { return Ok(None) };
    let dest = expanduser(&dest);
    let src = crate::config::load::resolve(src_at.file, &expanduser(&src_rel));

    // `mode`, `owner` and `group` mean what they do on `file` (§6.4), so they
    // are parsed by the same rules — including that an unquoted mode is an
    // error rather than a guess between YAML dialects.
    let meta = file::parse_metadata(step, engine, ctx, raw)?;
    let Some(meta) = meta else { return Ok(None) };

    let tree = src.is_dir();
    let mut outputs = Vec::new();

    if tree {
        for entry in walk(&src, src_at)? {
            let rel = entry.strip_prefix(&src).unwrap_or(&entry).to_path_buf();
            // The label names the file that changes, so the `.j2` comes off:
            // nothing called `top.conf.j2` is ever written.
            let rel = strip_j2(&rel);
            let out = Path::new(&dest).join(&rel);
            let Some(content) = render_file(&entry, engine, ctx) else { return Ok(None) };
            let label = rel.display().to_string();
            outputs.push(meta.clone().into_file(out.display().to_string(), content, Some(label)));
        }
    } else {
        let Some(content) = render_file(&src, engine, ctx) else { return Ok(None) };
        outputs.push(meta.into_file(dest.clone(), content, None));
    }

    Ok(Some(Spec { outputs, dest, tree }))
}

/// Walk the source tree in sorted order, depth first.
///
/// A symlink anywhere in it is a validation error: the tool does not follow
/// them, and silently skipping one would place a tree that is missing a file
/// nobody noticed was a link.
fn walk(root: &Path, at: N<'_>) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir)
            .map_err(|e| at.err(format!("cannot read {}: {e}", dir.display())))?;
        let mut sorted: Vec<PathBuf> = Vec::new();
        for e in entries {
            let e = e.map_err(|e| at.err(format!("cannot read {}: {e}", dir.display())))?;
            sorted.push(e.path());
        }
        sorted.sort();
        for p in sorted {
            let meta = std::fs::symlink_metadata(&p)
                .map_err(|e| at.err(format!("cannot read {}: {e}", p.display())))?;
            if meta.file_type().is_symlink() {
                return Err(at
                    .err(format!("{} is a symlink", p.display()))
                    .with_note("template does not follow symlinks; copy the target instead"));
            }
            if meta.is_dir() {
                stack.push(p);
            } else {
                out.push(p);
            }
        }
    }
    out.sort();
    Ok(out)
}

/// Render a source file, or copy it when it is not text.
///
/// Spec §6.4: a file that is not valid UTF-8 is placed byte for byte. The
/// validate walk already steps over these; apply still has to put them there.
fn render_file(path: &Path, engine: &Engine, ctx: &Value) -> Option<Vec<u8>> {
    let bytes = std::fs::read(path).ok()?;
    match String::from_utf8(bytes) {
        Ok(text) => engine.render(&text, ctx).ok().map(String::into_bytes),
        Err(e) => Some(e.into_bytes()),
    }
}

/// A `.j2` suffix is a marker for the reader, not part of the name.
fn strip_j2(rel: &Path) -> PathBuf {
    match rel.extension().and_then(|e| e.to_str()) {
        Some("j2") => rel.with_extension(""),
        _ => rel.to_path_buf(),
    }
}

// ── probe and apply ───────────────────────────────────────────────────────

impl Spec {
    pub fn plan(&self, ctx: &Ctx<'_>) -> (Effect, Option<String>) {
        self.reach(ctx, false)
    }

    pub fn apply(&self, ctx: &Ctx<'_>) -> (Effect, Option<String>) {
        self.reach(ctx, true)
    }

    /// One line for the whole step, and one diff per changed file underneath,
    /// each headed by its path relative to `src` (spec §6.4).
    fn reach(&self, ctx: &Ctx<'_>, act: bool) -> (Effect, Option<String>) {
        if let Some(bad) = self.dest_is_a_file() {
            return (
                Effect::Failed {
                    msg: format!("{bad} exists and is a regular file, not a directory"),
                    detail: String::new(),
                },
                None,
            );
        }

        let mut changed = 0usize;
        let mut diffs = Vec::new();
        for spec in &self.outputs {
            match if act { spec.apply(ctx) } else { spec.plan(ctx) } {
                Effect::Ok => {}
                Effect::Changed(detail) => {
                    changed += 1;
                    // The diff heads itself with the label (§6.4), so there
                    // is nothing to prepend.
                    if let Some(d) = detail {
                        diffs.push(d);
                    }
                }
                Effect::Unprobed => return (Effect::Unprobed, None),
                failed @ Effect::Failed { .. } => return (failed, None),
            }
        }

        if changed == 0 {
            return (Effect::Ok, None);
        }
        let note = self.tree.then(|| format!("{changed} of {}", self.outputs.len()));
        (Effect::Changed(Some(diffs.join("\n"))), note)
    }

    /// Directory mode needs `dest` to be a directory or absent. Rendering a
    /// tree over a regular file would place the last file and silently drop
    /// the rest.
    fn dest_is_a_file(&self) -> Option<&str> {
        if !self.tree {
            return None;
        }
        Path::new(&self.dest).is_file().then_some(self.dest.as_str())
    }
}
