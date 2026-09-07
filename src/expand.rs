//! The expansion walk: one sequential pass that turns a plan file into a flat
//! list of steps, evaluating scope as it goes.
//!
//! `validate` and `plan` are the same walk with different reporting, which is
//! the point — a plan cannot succeed on a config validate rejects. Phase 1
//! builds the runner on top of this by giving the actions something to do.

use crate::config::load::{self, Component, Loader};
use crate::config::model::{self, Step};
use crate::error::{Diag, Diags, Result};
use crate::scope::{Globals, Map, Scope};
use crate::template::Engine;
use crate::yaml::N;
use minijinja::Value;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Check everything; produce no step list.
    Validate { strict: bool },
    /// Check everything and report what would run. No probing yet (phase 0).
    Plan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// Excluded before rendering: by tag, or by a `when` that read false.
    Skipped(String),
    /// Would run, but nothing was probed, so no verdict is claimed.
    WouldRunUnprobed,
}

/// One step after expansion, ready to report or (from phase 1) to execute.
#[derive(Debug, Clone)]
pub struct Flat {
    pub name: String,
    pub file: PathBuf,
    pub depth: usize,
    pub status: Status,
}

pub struct Selection {
    pub tags: Vec<String>,
    pub skip_tags: Vec<String>,
}

impl Selection {
    /// Ansible semantics, deliberately (D7). A positive filter excludes
    /// untagged steps; `always` overrides it; `--skip-tags` always wins.
    fn selects(&self, tags: &BTreeSet<String>) -> bool {
        if tags.iter().any(|t| self.skip_tags.contains(t)) {
            return false;
        }
        if self.tags.is_empty() {
            return true;
        }
        tags.contains("always") || tags.iter().any(|t| self.tags.contains(t))
    }
}

pub struct Expander {
    loader: Loader,
    engine: Engine,
    globals: Rc<Globals>,
    mode: Mode,
    selection: Selection,
    pub diags: Diags,
    pub steps: Vec<Flat>,
    /// Names bound by `register`. At plan time they hold a placeholder, so a
    /// `when` that reads one cannot be trusted — the step is reported
    /// unprobed rather than skipped.
    registers: BTreeSet<String>,
    stack: Vec<PathBuf>,
}

impl Expander {
    pub fn new(globals: Rc<Globals>, mode: Mode, selection: Selection) -> Expander {
        Expander {
            loader: Loader::new(),
            engine: Engine::new(),
            globals,
            mode,
            selection,
            diags: Diags::new(),
            steps: Vec::new(),
            registers: BTreeSet::new(),
            stack: Vec::new(),
        }
    }

    pub fn run(&mut self, root: &Path) -> Result<()> {
        let mut scope = Scope::root(Rc::clone(&self.globals));
        let doc = self.loader.load(root)?;
        let (steps, errors) = model::parse_steps(doc.node())?;
        for d in errors {
            self.diags.push(d);
        }
        self.walk(&steps, &mut scope, 0, &BTreeSet::new());
        Ok(())
    }

    fn walk(&mut self, steps: &[Step<'static>], scope: &mut Scope, depth: usize, inherited: &BTreeSet<String>) {
        for step in steps {
            if let Err(d) = model::check_modifiers(step) {
                self.diags.push(d);
                continue;
            }
            self.step(step, scope, depth, inherited);
        }
    }

    fn step(&mut self, step: &Step<'static>, scope: &mut Scope, depth: usize, inherited: &BTreeSet<String>) {
        let mut tags = inherited.clone();
        if let Some(n) = step.mods.tags {
            match n.as_str_or_seq() {
                Ok(ts) => tags.extend(ts.iter().map(|s| s.to_string())),
                Err(d) => {
                    self.diags.push(d);
                    return;
                }
            }
        }

        // Spec §8: tag filtering happens before rendering, so an excluded step
        // cannot fail on a variable it never gets to read. This is mooncake
        // #191 and #196, and it is why the check is here and not below `when`.
        // Structural steps are exempt: they build the scope every later step
        // reads, and selecting work must not silently unset variables.
        if !step.is_structural() && !self.selection.selects(&tags) {
            self.record(step, scope, depth, tags, Status::Skipped("not selected by tags".into()));
            return;
        }

        match self.condition(step, scope) {
            Cond::False => {
                self.record(step, scope, depth, tags, Status::Skipped("when: false".into()));
                return;
            }
            Cond::Error(d) => {
                self.diags.push(d);
                return;
            }
            Cond::True | Cond::Unprobed => {}
        }

        match step.key {
            "vars" => self.do_vars(step, scope),
            "vars_file" => self.do_vars_file(step, scope),
            "import" => self.do_import(step, scope, depth, &tags),
            "use" => self.do_use(step, scope, depth, &tags),
            _ => self.do_action(step, scope, depth, tags),
        }
    }

    // ── conditions ────────────────────────────────────────────────────────

    fn condition(&mut self, step: &Step<'static>, scope: &Scope) -> Cond {
        let Some(when) = step.mods.when else { return Cond::True };
        let expr = match when.as_str() {
            Ok(s) => s,
            Err(d) => return Cond::Error(d),
        };
        // A `when` reading a registered result cannot be evaluated before the
        // step that registers it has run. Plan says so instead of guessing.
        if self.reads_a_register(expr) {
            return Cond::Unprobed;
        }
        let at = |m: String| when.err(m);
        match self.engine.eval_bool(expr, &scope.ctx(), &at) {
            Ok(true) => Cond::True,
            Ok(false) => Cond::False,
            Err(d) => Cond::Error(d),
        }
    }

    fn reads_a_register(&self, expr: &str) -> bool {
        if self.registers.is_empty() {
            return false;
        }
        // Top-level names only: `gitcfg.changed` reads the register `gitcfg`.
        match self.engine.undeclared(expr) {
            Some(names) => names.iter().any(|n| self.registers.contains(n)),
            None => false,
        }
    }

    // ── structural keywords ───────────────────────────────────────────────

    fn do_vars(&mut self, step: &Step<'static>, scope: &mut Scope) {
        let Some(pairs) = self.diags.absorb(step.body.as_map()) else { return };
        for (k, v) in pairs {
            let Some(name) = self.diags.absorb(k.as_scalar_string()) else { continue };
            let ctx = scope.ctx();
            match self.render_value(v, &ctx) {
                Ok(value) => scope.set(name, value),
                Err(d) => self.diags.push(d),
            }
        }
    }

    fn do_vars_file(&mut self, step: &Step<'static>, scope: &mut Scope) {
        let optional =
            step.mods.optional.map(|n| n.as_bool().unwrap_or(false)).unwrap_or(false);
        let Some(entries) = self.diags.absorb(step.body.as_str_or_seq_nodes()) else { return };

        for entry in entries {
            let ctx = scope.ctx();
            let Some(rel) = self.diags.absorb(self.render_str(entry, &ctx)) else { continue };
            let path = load::resolve(entry.file, &rel);
            if !path.exists() {
                if !optional {
                    self.diags.push(load::missing(entry, &path, "vars_file"));
                }
                continue;
            }
            let Some(doc) = self.diags.absorb(self.loader.load(&path)) else { continue };
            let root = doc.node();
            if root.is_null() {
                continue; // an empty vars file sets nothing, which is not an error
            }
            let Some(pairs) = self.diags.absorb(root.as_map().map_err(|_| {
                entry.err(format!(
                    "vars_file {} is {}, expected a mapping of variables",
                    path.display(),
                    root.kind()
                ))
            })) else {
                continue;
            };
            for (k, v) in pairs {
                let Some(name) = self.diags.absorb(k.as_scalar_string()) else { continue };
                // A vars file is data, not a template: values are taken as
                // written. Templating them would make `{{` in a config value
                // an error, and these files hold shell snippets.
                match v.to_value() {
                    Ok(value) => scope.set(name, value),
                    Err(d) => self.diags.push(d),
                }
            }
        }
    }

    fn do_import(&mut self, step: &Step<'static>, scope: &mut Scope, depth: usize, tags: &BTreeSet<String>) {
        let ctx = scope.ctx();
        let Some(rel) = self.diags.absorb(self.render_str(step.body, &ctx)) else { return };
        let path = load::resolve(step.body.file, &rel);
        if !path.exists() {
            self.diags.push(load::missing(step.body, &path, "import"));
            return;
        }
        let Some(_guard) = self.enter(&path, step.body) else { return };

        let Some(doc) = self.diags.absorb(self.loader.load(&path)) else {
            self.stack.pop();
            return;
        };
        match model::parse_steps(doc.node()) {
            // `import` shares the caller's scope (spec §3.1): variables set in
            // the imported file are visible after it.
            Ok((steps, errors)) => {
                for d in errors {
                    self.diags.push(d);
                }
                self.walk(&steps, scope, depth + 1, tags);
            }
            Err(d) => self.diags.push(d),
        }
        self.stack.pop();
    }

    fn do_use(&mut self, step: &Step<'static>, scope: &mut Scope, depth: usize, tags: &BTreeSet<String>) {
        let ctx = scope.ctx();
        let Some(rel) = self.diags.absorb(self.render_str(step.body, &ctx)) else { return };
        let path = load::resolve(step.body.file, &rel);
        if !path.exists() {
            self.diags.push(load::missing(step.body, &path, "use"));
            return;
        }
        let Some(_guard) = self.enter(&path, step.body) else { return };

        let Some(doc) = self.diags.absorb(self.loader.load(&path)) else {
            self.stack.pop();
            return;
        };
        let Some(component) = self.diags.absorb(load::parse_component(doc)) else {
            self.stack.pop();
            return;
        };

        match self.bind_props(step, &component, scope) {
            Ok(props) => {
                // `use` runs in a child scope: the parent is visible read-only
                // and the component's own vars do not leak back (spec §3.1).
                let mut child = scope.child_with_props(props);
                self.walk(&component.steps, &mut child, depth + 1, tags);
            }
            Err(ds) => {
                for d in ds {
                    self.diags.push(d);
                }
            }
        }
        self.stack.pop();
    }

    /// Spec §3.2: unknown prop, missing required prop, and wrong type after
    /// rendering are all errors — at validate time, not at apply time.
    fn bind_props(
        &mut self,
        step: &Step<'static>,
        component: &Component,
        scope: &Scope,
    ) -> std::result::Result<Map, Vec<Diag>> {
        let mut errors = Vec::new();
        let mut out = Map::new();
        let ctx = scope.ctx();

        let given = match step.mods.props {
            Some(n) => match n.as_map() {
                Ok(pairs) => pairs,
                Err(d) => return Err(vec![d]),
            },
            None => Vec::new(),
        };

        for (k, v) in &given {
            let name = match k.as_scalar_string() {
                Ok(n) => n,
                Err(d) => {
                    errors.push(d);
                    continue;
                }
            };
            let Some(schema) = component.prop(&name) else {
                let known: Vec<&str> = component.props.iter().map(|p| p.name.as_str()).collect();
                errors.push(
                    k.err(format!(
                        "component {} has no prop `{name}`",
                        rel_display(&component.path)
                    ))
                    .with_note(if known.is_empty() {
                        "it declares no props".to_string()
                    } else {
                        format!("it declares: {}", known.join(", "))
                    }),
                );
                continue;
            };
            match self.render_value(*v, &ctx) {
                Ok(value) => {
                    if !schema.ty.accepts(&value) {
                        errors.push(v.err(format!(
                            "prop `{name}` is declared {} but got {}",
                            schema.ty.name(),
                            describe_value(&value)
                        )));
                        continue;
                    }
                    out.insert(name, value);
                }
                Err(d) => errors.push(d),
            }
        }

        for schema in &component.props {
            if out.contains_key(&schema.name) {
                continue;
            }
            match schema.default {
                Some(d) => match d.to_value() {
                    Ok(v) => {
                        out.insert(schema.name.clone(), v);
                    }
                    Err(e) => errors.push(e),
                },
                None if schema.required => errors.push(
                    step.at
                        .err(format!("missing required prop `{}`", schema.name))
                        .with_note(format!(
                            "declared at {}:{}",
                            rel_display(&component.path),
                            schema.at.line()
                        )),
                ),
                None => {
                    // Declared, not required, no default: absent is a value.
                    out.insert(schema.name.clone(), Value::from(()));
                }
            }
        }

        if errors.is_empty() { Ok(out) } else { Err(errors) }
    }

    // ── actions ───────────────────────────────────────────────────────────

    fn do_action(&mut self, step: &Step<'static>, scope: &mut Scope, depth: usize, tags: BTreeSet<String>) {
        let ctx = scope.ctx();

        // Render every string field. This is where an undefined variable in a
        // rarely-taken branch surfaces, which is the point of strict mode.
        let raw = step.mods.raw.map(|n| n.as_bool().unwrap_or(false)).unwrap_or(false);
        let mut fields: Vec<N<'static>> = Vec::new();
        if !raw {
            collect_strings(step.body, &mut fields);
        }
        for n in [step.mods.unless, step.mods.creates, step.mods.cwd, step.mods.env]
            .into_iter()
            .flatten()
        {
            collect_strings(n, &mut fields);
        }
        for f in fields {
            if let Err(d) = self.render_str(f, &ctx) {
                self.diags.push(d);
            }
        }

        // Compile the expression modifiers. `result` is only in scope at apply
        // time, so syntax is all that can be checked here.
        for e in model::expression_fields(step) {
            if let Ok(src) = e.as_str() {
                let at = |m: String| e.err(m);
                if let Err(d) = self.engine.check_syntax(src, &at) {
                    self.diags.push(d);
                }
            }
        }

        if step.key == "template" {
            self.check_template_sources(step, &ctx);
        }
        if step.key == "file" {
            self.check_file_source(step, &ctx);
        }

        if let Some(reg) = step.mods.register.and_then(|n| n.as_str().ok()) {
            self.registers.insert(reg.to_string());
            scope.set(reg, placeholder_result());
        }

        if let Mode::Validate { strict: true } = self.mode {
            self.check_strict_gate(step);
        }

        self.record(step, scope, depth, tags, Status::WouldRunUnprobed);
    }

    /// The gate for `validate --strict` (D3): a `shell` or `cmd` step with no
    /// declared idempotency is `unknown` forever.
    fn check_strict_gate(&mut self, step: &Step<'static>) {
        if !matches!(step.key, "shell" | "cmd") {
            return;
        }
        let gated = step.mods.unless.is_some()
            || step.mods.creates.is_some()
            || step.mods.changed_when.is_some();
        if !gated {
            self.diags.push(
                step.at
                    .err(format!("`{}` step has no idempotency gate", step.key))
                    .with_note("add `unless:`, `creates:`, or `changed_when:`"),
            );
        }
    }

    /// Render the `.j2` sources a `template` step names, so an undefined
    /// variable inside a template is an error at plan time, not at apply time.
    /// Directory mode (spec §6.4) renders every file under `src`.
    fn check_template_sources(&mut self, step: &Step<'static>, ctx: &Value) {
        let Some(src_at) = step.body.get("src") else {
            self.diags.push(step.body.err("`template` requires `src`"));
            return;
        };
        let Some(rel) = self.diags.absorb(self.render_str(src_at, ctx)) else { return };
        let path = load::resolve(src_at.file, &rel);
        if !path.exists() {
            self.diags.push(load::missing(src_at, &path, "template src"));
            return;
        }
        let files = if path.is_dir() {
            match collect_tree(&path) {
                Ok(f) => f,
                Err(e) => {
                    self.diags.push(src_at.err(format!("cannot walk {}: {e}", path.display())));
                    return;
                }
            }
        } else {
            vec![path]
        };
        for f in files {
            let Ok(text) = std::fs::read_to_string(&f) else {
                continue; // a binary asset in the tree is copied, not rendered
            };
            if let Err(e) = self.engine.render(&text, ctx) {
                self.diags.push(
                    src_at
                        .err(format!(
                            "{}: {}",
                            rel_display(&f),
                            self.engine.describe_with(&text, ctx, &e)
                        ))
                        .with_note(match e.line() {
                            Some(l) => format!("in the template at line {l}"),
                            None => "in the template".to_string(),
                        }),
                );
            }
        }
    }

    fn check_file_source(&mut self, step: &Step<'static>, ctx: &Value) {
        let Some(src_at) = step.body.get("src") else { return };
        let Some(rel) = self.diags.absorb(self.render_str(src_at, ctx)) else { return };
        // A `link` target need not exist on this machine.
        let is_link = step
            .body
            .get("state")
            .and_then(|s| s.as_str().ok())
            .map(|s| s == "link")
            .unwrap_or(false);
        if is_link {
            return;
        }
        let path = load::resolve(src_at.file, &rel);
        if !path.exists() {
            self.diags.push(load::missing(src_at, &path, "file src"));
        }
    }

    // ── helpers ───────────────────────────────────────────────────────────

    fn record(&mut self, step: &Step<'static>, scope: &Scope, depth: usize, tags: BTreeSet<String>, status: Status) {
        if let Mode::Validate { .. } = self.mode {
            return;
        }
        let _ = tags;
        let name = self.step_name(step, scope, &status);
        self.steps.push(Flat {
            name,
            file: step.at.file.to_path_buf(),
            depth,
            status,
        });
    }

    /// Spec §10: a `name` that renders empty falls back to the action key plus
    /// its first argument. A skipped step is not rendered at all, so it uses
    /// the raw name — it may reference a variable that does not exist.
    fn step_name(&mut self, step: &Step<'static>, scope: &Scope, status: &Status) -> String {
        let Some(n) = step.mods.name else { return step.fallback_name() };
        let Ok(raw) = n.as_str() else { return step.fallback_name() };
        if matches!(status, Status::Skipped(_)) {
            return raw.to_string();
        }
        match self.engine.render(raw, &scope.ctx()) {
            Ok(s) if !s.trim().is_empty() => s,
            Ok(_) => step.fallback_name(),
            Err(e) => {
                let msg = self.engine.describe_with(raw, &scope.ctx(), &e);
                self.diags.push(n.err(msg));
                raw.to_string()
            }
        }
    }

    fn render_str(&self, at: N<'static>, ctx: &Value) -> Result<String> {
        let s = at.as_str()?;
        self.engine.render(s, ctx).map_err(|e| at.err(self.engine.describe_with(s, ctx, &e)))
    }

    fn render_value(&self, at: N<'static>, ctx: &Value) -> Result<Value> {
        match at.as_str() {
            Ok(s) => self
                .engine
                .render_field(s, ctx)
                .map_err(|e| at.err(self.engine.describe_with(s, ctx, &e))),
            // Non-strings are taken as written, but their strings are rendered.
            Err(_) => match at.as_seq() {
                Ok(items) => {
                    let vs: Result<Vec<Value>> =
                        items.into_iter().map(|i| self.render_value(i, ctx)).collect();
                    Ok(Value::from(vs?))
                }
                Err(_) => match at.as_map() {
                    Ok(pairs) => {
                        let mut m = std::collections::BTreeMap::new();
                        for (k, v) in pairs {
                            m.insert(k.as_scalar_string()?, self.render_value(v, ctx)?);
                        }
                        Ok(Value::from(m))
                    }
                    Err(_) => at.to_value(),
                },
            },
        }
    }

    /// Push a file onto the include stack, reporting a cycle by naming it.
    fn enter(&mut self, path: &Path, at: N<'static>) -> Option<()> {
        if let Some(i) = self.stack.iter().position(|p| p == path) {
            let mut chain: Vec<String> =
                self.stack[i..].iter().map(|p| rel_display(p)).collect();
            chain.push(rel_display(path));
            self.diags.push(
                at.err("import cycle")
                    .with_note(chain.join(" → ")),
            );
            return None;
        }
        self.stack.push(path.to_path_buf());
        Some(())
    }
}

enum Cond {
    True,
    False,
    Unprobed,
    Error(Diag),
}

/// What a `register` holds at plan time. Real shape (spec §4), empty values,
/// so `when: r.changed` reads false and the reader is told it is unprobed.
fn placeholder_result() -> Value {
    let mut m = std::collections::BTreeMap::new();
    m.insert("rc".to_string(), Value::from(0));
    m.insert("stdout".to_string(), Value::from(""));
    m.insert("stderr".to_string(), Value::from(""));
    m.insert("changed".to_string(), Value::from(false));
    m.insert("skipped".to_string(), Value::from(false));
    Value::from(m)
}

fn describe_value(v: &Value) -> String {
    use minijinja::value::ValueKind as Kind;
    match v.kind() {
        Kind::String => format!("the string `{v}`"),
        Kind::Number => format!("the number {v}"),
        Kind::Bool => format!("the boolean {v}"),
        Kind::Seq => "a list".to_string(),
        Kind::Map => "a mapping".to_string(),
        Kind::None | Kind::Undefined => "nothing".to_string(),
        _ => format!("`{v}`"),
    }
}

fn collect_strings(n: N<'static>, out: &mut Vec<N<'static>>) {
    if n.as_str().is_ok() {
        out.push(n);
    } else if let Ok(items) = n.as_seq() {
        for i in items {
            collect_strings(i, out);
        }
    } else if let Ok(pairs) = n.as_map() {
        for (_, v) in pairs {
            collect_strings(v, out);
        }
    }
}

fn collect_tree(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let mut entries: Vec<_> = std::fs::read_dir(&d)?.collect::<std::io::Result<_>>()?;
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push(p);
            }
        }
    }
    out.sort();
    Ok(out)
}

fn rel_display(p: &Path) -> String {
    std::env::current_dir()
        .ok()
        .and_then(|cwd| p.strip_prefix(cwd).ok().map(|r| r.display().to_string()))
        .unwrap_or_else(|| p.display().to_string())
}
