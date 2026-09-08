//! The expansion walk: one sequential pass that turns a plan file into a flat
//! list of steps, evaluating scope as it goes.
//!
//! `validate`, `plan` and `apply` are one walk with three settings. That is
//! the point: a plan cannot succeed on a config validate rejects, and apply
//! cannot run a step plan never showed. Execution is interleaved rather than
//! bolted on afterwards, because a `when` that reads a `register` needs the
//! registering step to have actually run (D14).

use crate::actions::Action;
use crate::config::load::{self, Component, Loader};
use crate::config::model::{self, Step};
use crate::error::{Diag, Diags, Result};
use crate::exec::process::Output;
use crate::exec::runner::{Judge, Prepared, Runner};
use crate::output::Sink;
use crate::output::event::{Event, Status, Summary};
use crate::scope::{Globals, Map, Scope};
use crate::template::{Engine, expanduser};
use crate::yaml::N;
use minijinja::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

/// Spec §4: a step with no `timeout` gets ten minutes.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    /// Check everything; run nothing; produce no step list.
    Validate { strict: bool },
    /// Check everything and report what would run. `probe` runs the read-only
    /// questions — `unless`, `creates`, `assert` — and is off under
    /// `--plan-no-probe`, where no verdict is claimed at all.
    Plan { probe: bool },
    /// Check everything, then do it.
    Apply,
    /// `apply` with a component as the root (D17). Identical in every
    /// respect but one: an ungated `shell`/`cmd` that exits 0 is `ok`, not
    /// `unknown`. In a task the step's contract is its exit code, and there
    /// is no state for provision to be unsure about.
    Run,
}

impl Mode {
    fn executes(&self) -> bool {
        matches!(self, Mode::Apply | Mode::Run)
    }
    fn reports(&self) -> bool {
        !matches!(self, Mode::Validate { .. })
    }
}

pub(crate) struct Selection {
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

pub(crate) struct Expander {
    loader: Loader,
    engine: Engine,
    globals: Rc<Globals>,
    mode: Mode,
    selection: Selection,
    pub diags: Diags,
    pub summary: Summary,
    /// Set by the walk `apply` runs before executing anything, so the sudo
    /// preflight can happen before the first step (spec §7).
    pub needs_sudo: bool,
    /// Names bound by `register`. At plan time they hold a placeholder, so a
    /// `when` that reads one cannot be trusted — the step is reported
    /// unprobed rather than skipped.
    registers: BTreeSet<String>,
    stack: Vec<PathBuf>,
    runner: Option<Runner>,
    sink: Box<dyn Sink>,
    index: usize,
    /// Spec §2: apply stops at the first failure. Nothing after it is run,
    /// reported, or rendered.
    stopped: bool,
    /// Spec §8 `--keep-going`: carry on past a failed step, so one run
    /// reports everything broken instead of the first thing.
    keep_going: bool,
}

impl Expander {
    pub(crate) fn new(globals: Rc<Globals>, mode: Mode, selection: Selection) -> Expander {
        Expander {
            loader: Loader::new(),
            engine: Engine::new(),
            globals,
            mode,
            selection,
            diags: Diags::new(),
            summary: Summary::default(),
            needs_sudo: false,
            registers: BTreeSet::new(),
            stack: Vec::new(),
            runner: None,
            sink: Box::new(crate::output::Silent),
            index: 0,
            stopped: false,
            keep_going: false,
        }
    }

    /// Spec §8. `apply` only: `plan` and `validate` never stopped at a
    /// failure, so there is nothing for the flag to change there.
    pub(crate) fn keep_going(mut self, yes: bool) -> Expander {
        self.keep_going = yes;
        self
    }

    pub(crate) fn with_runner(mut self, runner: Runner) -> Expander {
        self.runner = Some(runner);
        self
    }

    pub(crate) fn with_sink(mut self, sink: Box<dyn Sink>) -> Expander {
        self.sink = sink;
        self
    }

    /// Emitted once at the end, after the last step's line.
    pub(crate) fn summarize(&mut self, plan: &Path, elapsed: Duration) {
        self.summary.duration = elapsed;
        let s = self.summary.clone();
        self.sink.summary(plan, &s);
    }

    pub(crate) fn run(&mut self, root: &Path) -> Result<()> {
        let mut scope = Scope::root(Rc::clone(&self.globals));
        let doc = self.loader.load(root)?;
        let (steps, errors) = model::parse_steps(doc.node())?;
        for d in errors {
            self.diags.push(d);
        }
        self.walk(&steps, &mut scope, 0, &BTreeSet::new());
        Ok(())
    }

    /// D17: the root is a component, not a plan. Its props come from the
    /// command line rather than a `use` site; everything below is the same
    /// walk. Returns without walking when a prop is wrong — the diagnostics
    /// are in `self.diags`, and running half a task with a bad argument is
    /// worse than running none of it.
    pub(crate) fn run_component(&mut self, root: &Path, given: &[(String, String)]) -> Result<()> {
        let doc = self.loader.load(root)?;
        let component = load::parse_component(doc)?;
        let props = match self.cli_props(&component, given) {
            Ok(p) => p,
            Err(ds) => {
                for d in ds {
                    self.diags.push(d);
                }
                return Ok(());
            }
        };
        let dir = component.path.parent().unwrap_or(Path::new("."));
        let mut scope = Scope::root(Rc::clone(&self.globals)).child_with_props(props, dir);
        self.walk(&component.steps, &mut scope, 0, &BTreeSet::new());
        Ok(())
    }

    /// D17 `run --step`: steps that came from somewhere other than a file,
    /// walked in a scope holding only facts and the command line.
    pub(crate) fn run_steps(&mut self, steps: &[Step<'static>]) -> Result<()> {
        let mut scope = Scope::root(Rc::clone(&self.globals));
        self.walk(steps, &mut scope, 0, &BTreeSet::new());
        Ok(())
    }

    fn walk(
        &mut self,
        steps: &[Step<'static>],
        scope: &mut Scope,
        depth: usize,
        inherited: &BTreeSet<String>,
    ) {
        for step in steps {
            if self.stopped {
                return;
            }
            if let Err(d) = model::check_modifiers(step) {
                self.diags.push(d);
                continue;
            }
            self.step(step, scope, depth, inherited);
        }
    }

    fn step(
        &mut self,
        step: &Step<'static>,
        scope: &mut Scope,
        depth: usize,
        inherited: &BTreeSet<String>,
    ) {
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
            let status = Status::Skipped("not selected by tags".into());
            self.bind_register(step, scope, None, &status);
            self.record(step, scope, depth, tags, status);
            return;
        }

        match self.condition(step, scope) {
            Cond::False => {
                let status = Status::Skipped("when: false".into());
                self.bind_register(step, scope, None, &status);
                self.record(step, scope, depth, tags, status);
                return;
            }
            Cond::Error(d) => {
                self.diags.push(d);
                return;
            }
            Cond::Unprobed => {
                // D14. The step that registers this value has not run, so the
                // condition has no answer yet — and `condition()` worked that
                // out and this arm used to throw it away, letting plan probe
                // the step and report a verdict it cannot have. A `service:
                // {state: restarted}` gated on a unit file that has not been
                // deployed yet came out as `would change`, which is a claim
                // about a machine nobody asked.
                //
                // `validate` must still walk in: it renders the fields and
                // parses the action body and runs nothing, and skipping that
                // would put back the hole d502ed3 closed. `apply` never
                // reaches here — by then the register holds a real result,
                // which is what `reads_a_register` checks first.
                //
                // Structural steps are exempt. An `import` has no verdict of
                // its own, and reporting a whole subtree as unprobed would
                // hide far more than the honesty buys.
                if self.mode.reports() && !step.is_structural() {
                    self.bind_register(step, scope, None, &Status::WouldRunUnprobed);
                    self.record(step, scope, depth, tags, Status::WouldRunUnprobed);
                    return;
                }
            }
            Cond::True => {}
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
        let Some(when) = step.mods.when else {
            return Cond::True;
        };
        let expr = match when.as_scalar_string() {
            Ok(s) => s,
            Err(d) => return Cond::Error(d),
        };
        let expr = expr.as_str();
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
        // At apply time the registering step has actually run, so the value is
        // the real one and the condition is answerable. This guard is the
        // whole difference between D14's placeholder and a real result.
        if self.mode.executes() || self.registers.is_empty() {
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
        let Some(pairs) = self.diags.absorb(step.body.as_map()) else {
            return;
        };
        for (k, v) in pairs {
            let Some(name) = self.diags.absorb(k.as_scalar_string()) else {
                continue;
            };
            let ctx = scope.ctx();
            match self.engine.render_node(v, &ctx) {
                Ok(value) => scope.set(name, value),
                Err(d) => self.diags.push(d),
            }
        }
    }

    fn do_vars_file(&mut self, step: &Step<'static>, scope: &mut Scope) {
        let optional = step
            .mods
            .optional
            .map(|n| n.as_bool().unwrap_or(false))
            .unwrap_or(false);
        let Some(entries) = self.diags.absorb(step.body.as_str_or_seq_nodes()) else {
            return;
        };

        for entry in entries {
            let ctx = scope.ctx();
            let Some(rel) = self.diags.absorb(self.render_str(entry, &ctx)) else {
                continue;
            };
            let path = load::resolve(entry.file, &rel);
            if !path.exists() {
                if !optional {
                    self.diags.push(load::missing(entry, &path, "vars_file"));
                }
                continue;
            }
            let Some(doc) = self.diags.absorb(self.loader.load(&path)) else {
                continue;
            };
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
                let Some(name) = self.diags.absorb(k.as_scalar_string()) else {
                    continue;
                };
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

    fn do_import(
        &mut self,
        step: &Step<'static>,
        scope: &mut Scope,
        depth: usize,
        tags: &BTreeSet<String>,
    ) {
        let ctx = scope.ctx();
        let Some(rel) = self.diags.absorb(self.render_str(step.body, &ctx)) else {
            return;
        };
        let path = load::resolve(step.body.file, &rel);
        if !path.exists() {
            self.diags.push(load::missing(step.body, &path, "import"));
            return;
        }
        let Some(_guard) = self.enter(&path, step.body) else {
            return;
        };

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

    fn do_use(
        &mut self,
        step: &Step<'static>,
        scope: &mut Scope,
        depth: usize,
        tags: &BTreeSet<String>,
    ) {
        let ctx = scope.ctx();
        let Some(rel) = self.diags.absorb(self.render_str(step.body, &ctx)) else {
            return;
        };
        let path = load::resolve(step.body.file, &rel);
        if !path.exists() {
            self.diags.push(load::missing(step.body, &path, "use"));
            return;
        }
        let Some(_guard) = self.enter(&path, step.body) else {
            return;
        };

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
                let dir = component.path.parent().unwrap_or(Path::new("."));
                let mut child = scope.child_with_props(props, dir);
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
            match self.engine.render_node(*v, &ctx) {
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

        fill_defaults(component, &mut out, &mut errors, &|schema| {
            step.at
                .err(format!("missing required prop `{}`", schema.name))
        });

        if errors.is_empty() {
            Ok(out)
        } else {
            Err(errors)
        }
    }

    /// D17: the same three checks as a `use` site, against `--prop k=v`
    /// instead of a YAML mapping. Values are rendered as templates, so
    /// `--prop n='{{ 3 }}'` is the int 3 and `--prop n=3` is the string
    /// "3" — the sole-expression rule of §3.4, reaching the command line.
    fn cli_props(
        &mut self,
        component: &Component,
        given: &[(String, String)],
    ) -> std::result::Result<Map, Vec<Diag>> {
        let mut errors = Vec::new();
        let mut out = Map::new();
        let scope = Scope::root(Rc::clone(&self.globals));
        let ctx = scope.ctx();

        for (name, raw) in given {
            let Some(schema) = component.prop(name) else {
                let known: Vec<&str> = component.props.iter().map(|p| p.name.as_str()).collect();
                errors.push(
                    Diag::file_level(
                        "--prop",
                        format!(
                            "component {} has no prop `{name}`",
                            rel_display(&component.path)
                        ),
                    )
                    .with_note(if known.is_empty() {
                        "it declares no props".to_string()
                    } else {
                        format!("it declares: {}", known.join(", "))
                    }),
                );
                continue;
            };
            match self.engine.render(raw, &ctx) {
                Ok(text) => match typed_prop(schema, &text) {
                    Ok(value) => {
                        out.insert(name.clone(), value);
                    }
                    Err(d) => errors.push(d),
                },
                Err(e) => errors.push(Diag::file_level("--prop", e.to_string())),
            }
        }

        fill_defaults(component, &mut out, &mut errors, &|schema| {
            Diag::file_level("--prop", format!("missing required prop `{}`", schema.name))
        });

        if errors.is_empty() {
            Ok(out)
        } else {
            Err(errors)
        }
    }

    // ── actions ───────────────────────────────────────────────────────────

    fn do_action(
        &mut self,
        step: &Step<'static>,
        scope: &mut Scope,
        depth: usize,
        tags: BTreeSet<String>,
    ) {
        let ctx = scope.ctx();

        // Render every string field. This is where an undefined variable in a
        // rarely-taken branch surfaces, which is the point of strict mode. It
        // is also the only place that reports render errors: `prepare` below
        // renders the same nodes again and stays quiet, so one bad `{{ … }}`
        // is one diagnostic rather than two.
        let raw = step
            .mods
            .raw
            .map(|n| n.as_bool().unwrap_or(false))
            .unwrap_or(false);
        let mut fields: Vec<N<'static>> = Vec::new();
        if !raw {
            collect_strings(step.body, &mut fields);
        }
        for n in [
            step.mods.unless,
            step.mods.creates,
            step.mods.cwd,
            step.mods.env,
        ]
        .into_iter()
        .flatten()
        {
            collect_strings(n, &mut fields);
        }
        let mut renderable = true;
        for f in fields {
            if let Err(d) = self.render_str(f, &ctx) {
                self.diags.push(d);
                renderable = false;
            }
        }

        // Compile the expression modifiers. `result` is only in scope at apply
        // time, so syntax is all that can be checked here.
        for e in model::expression_fields(step) {
            // `as_scalar_string`, not `as_str`: YAML types `changed_when:
            // false` as a boolean, and `false` is a perfectly good expression
            // once it is text. Reading it as a string silently dropped it —
            // and a dropped `changed_when` does not mean "no opinion", it
            // means the step reports `changed` forever.
            match e.as_scalar_string() {
                Ok(src) => {
                    let at = |m: String| e.err(m);
                    if let Err(d) = self.engine.check_syntax(&src, &at) {
                        self.diags.push(d);
                        renderable = false;
                    }
                }
                Err(d) => {
                    self.diags.push(d);
                    renderable = false;
                }
            }
        }

        if let Some(allowed) = model::action_body_keys(step.key)
            && step.body.as_map().is_ok()
            && let Err(d) = step
                .body
                .deny_unknown_keys(allowed, &format!("`{}`", step.key))
        {
            self.diags.push(d);
            renderable = false;
        }

        if step.key == "template" {
            self.check_template_sources(step, &ctx);
        }
        if step.key == "file" {
            self.check_file_source(step, &ctx);
        }

        if step
            .mods
            .sudo
            .map(|n| n.as_bool().unwrap_or(false))
            .unwrap_or(false)
        {
            self.needs_sudo = true;
        }

        if let Mode::Validate { strict: true } = self.mode {
            self.check_strict_gate(step);
        }

        if !self.mode.reports() {
            // Parse the action body here too, and throw the result away.
            //
            // Without this, `validate` accepts what `plan` rejects: an action
            // body is only parsed on the way to running it, so a `pkg` with no
            // package name or a `file` with neither `content` nor `src` passes
            // validation and fails on the next command. That happened twice on
            // the day `pkg` landed, in this repo's own example and in the
            // fleet. §8 promises validate is the subset of plan that runs
            // nothing, not a weaker check.
            if renderable && let Err(d) = Action::parse(&self.engine, step, &ctx, raw) {
                self.diags.push(d);
            }
            // `validate` binds the placeholder so a later `when` that reads
            // this register still compiles. Nothing runs.
            self.bind_register(step, scope, None, &Status::WouldRunUnprobed);
            return;
        }

        // A step whose fields would not render cannot be run or judged, and
        // the diagnostic already says why.
        let prepared = if renderable {
            self.prepare(step, &ctx, raw)
        } else {
            None
        };
        let Some(prepared) = prepared else {
            self.bind_register(step, scope, None, &Status::WouldRunUnprobed);
            self.record(step, scope, depth, tags, Status::WouldRunUnprobed);
            return;
        };

        let unprobed = matches!(self.mode, Mode::Plan { probe: false });
        if unprobed {
            self.bind_register(step, scope, None, &Status::WouldRunUnprobed);
            self.record(step, scope, depth, tags, Status::WouldRunUnprobed);
            return;
        }

        let name = self.step_name(step, scope, &Status::WouldRunUnprobed);
        self.sink.start(&name, depth);

        let started = Instant::now();
        let done = {
            let judge = StepJudge {
                engine: &self.engine,
                base: scope.ctx_map(),
                // Non-scalars already produced a diagnostic above and never
                // reach here, so this cannot silently discard an opinion.
                failed_when: step
                    .mods
                    .failed_when
                    .and_then(|n| n.as_scalar_string().ok()),
                changed_when: step
                    .mods
                    .changed_when
                    .and_then(|n| n.as_scalar_string().ok()),
                at: step.at,
            };
            let runner = self
                .runner
                .as_ref()
                .expect("plan and apply always attach a runner");
            if self.mode.executes() {
                runner.apply(&prepared, &judge)
            } else {
                runner.probe(&prepared, &judge)
            }
        };
        let elapsed = started.elapsed();

        let done = match done {
            Ok(d) => d,
            Err(d) => {
                self.diags.push(d);
                self.stopped = true;
                return;
            }
        };

        self.bind_register(step, scope, done.out.as_ref(), &done.status);

        // Spec §8: apply stops at the first failure; plan never does. A plan
        // walk has not done the work, so an assert about work not yet done is
        // information, not a reason to hide every step after it.
        //
        // `--keep-going` carries on past a failed step, but never past an
        // interrupt: Ctrl-C is the operator saying stop, not a step saying it
        // could not do its work.
        if done.status.failed() && self.mode.executes() && (!self.keep_going || done.interrupted())
        {
            self.stopped = true;
        }
        self.emit(step, scope, depth, &tags, done, elapsed);
    }

    /// Turn a checked step into something the runner can execute.
    ///
    /// Renders are silent here on purpose: every node this touches was already
    /// rendered once above, and anything that failed was reported there.
    fn prepare(&mut self, step: &Step<'static>, ctx: &Value, raw: bool) -> Option<Prepared> {
        let action = match Action::parse(&self.engine, step, ctx, raw) {
            Ok(Some(a)) => a,
            // A field would not render. The sweep above already said so, with
            // a position; saying it twice is one typo, two diagnostics.
            Ok(None) => return None,
            Err(d) => {
                self.diags.push(d);
                return None;
            }
        };

        let text = |n: Option<N<'static>>| -> Option<String> {
            let n = n?;
            let src = n.as_str().ok()?;
            self.engine.render(src, ctx).ok()
        };

        let mut env = BTreeMap::new();
        if let Some(n) = step.mods.env {
            for (k, v) in n.as_map().ok()? {
                let key = k.as_scalar_string().ok()?;
                let val = v
                    .as_str()
                    .ok()
                    .and_then(|s| self.engine.render(s, ctx).ok());
                env.insert(
                    key,
                    val.unwrap_or_else(|| v.to_value().map(|x| x.to_string()).unwrap_or_default()),
                );
            }
        }

        // Spec §4: `cwd` defaults to the directory of the file the step is in,
        // never the process cwd — the same rule every path in a plan follows.
        let cwd = match text(step.mods.cwd) {
            Some(dir) => Some(load::resolve(step.at.file, &expanduser(&dir))),
            // `Path::new("x1.yml").parent()` is `Some("")`, not `None`, and
            // `current_dir("")` is ENOENT — which surfaced as the baffling
            // "cannot run `bash`: No such file or directory". So
            // `provision apply x1.yml` failed where `./x1.yml` worked. When
            // the plan is a bare filename its directory *is* the process cwd,
            // so inheriting is both simpler and right.
            // Under `run` the default is the invocation directory instead,
            // for every step including one inside a `use`d component. A task
            // is a command in the repo the operator is standing in; a shared
            // gate checked out under ~/.cache would otherwise `cd` to its own
            // toplevel and gate itself. `None` is exactly "inherit the
            // process cwd", so there is nothing to compute.
            None if matches!(self.mode, Mode::Run) => None,
            None => step
                .at
                .file
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .map(Path::to_path_buf),
        };

        Some(Prepared {
            action,
            unless: text(step.mods.unless),
            creates: text(step.mods.creates),
            cwd,
            env,
            sudo: step
                .mods
                .sudo
                .map(|n| n.as_bool().unwrap_or(false))
                .unwrap_or(false),
            timeout: step
                .mods
                .timeout
                .and_then(|n| model::parse_duration(n).ok())
                .unwrap_or(DEFAULT_TIMEOUT),
            retry: step.mods.retry.and_then(|n| model::parse_retry(n).ok()),
            has_changed_when: step.mods.changed_when.is_some(),
        })
    }

    /// Spec §4: `register` binds `{rc, stdout, stderr, changed, skipped}`. It
    /// binds on a skip too — that is what the `skipped` field is for, and a
    /// later `when: r.skipped` must not fail on an undefined name.
    fn bind_register(
        &mut self,
        step: &Step<'static>,
        scope: &mut Scope,
        out: Option<&Output>,
        status: &Status,
    ) {
        let Some(reg) = step.mods.register.and_then(|n| n.as_str().ok()) else {
            return;
        };
        self.registers.insert(reg.to_string());
        scope.set(reg, result_value(out, status));
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
        let Some(rel) = self.diags.absorb(self.render_str(src_at, ctx)) else {
            return;
        };
        let path = load::resolve(src_at.file, &rel);
        if !path.exists() {
            self.diags
                .push(load::missing(src_at, &path, "template src"));
            return;
        }
        let files = if path.is_dir() {
            match collect_tree(&path) {
                Ok(f) => f,
                Err(e) => {
                    self.diags
                        .push(src_at.err(format!("cannot walk {}: {e}", path.display())));
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
        let Some(src_at) = step.body.get("src") else {
            return;
        };
        let Some(rel) = self.diags.absorb(self.render_str(src_at, ctx)) else {
            return;
        };
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

    /// A step that reached a verdict without running: skipped, or unprobed.
    fn record(
        &mut self,
        step: &Step<'static>,
        scope: &Scope,
        depth: usize,
        tags: BTreeSet<String>,
        status: Status,
    ) {
        let _ = tags;
        self.finish(
            step,
            scope,
            depth,
            status,
            None,
            1,
            1,
            Duration::ZERO,
            None,
            None,
        );
    }

    /// A step the runner actually reached.
    fn emit(
        &mut self,
        step: &Step<'static>,
        scope: &Scope,
        depth: usize,
        tags: &BTreeSet<String>,
        done: crate::exec::runner::Done,
        elapsed: Duration,
    ) {
        let _ = tags;
        let (detail, note) = (done.detail.clone(), done.note.clone());
        self.finish(
            step,
            scope,
            depth,
            done.status,
            done.out.as_ref(),
            done.attempt,
            done.attempts,
            elapsed,
            detail,
            note,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn finish(
        &mut self,
        step: &Step<'static>,
        scope: &Scope,
        depth: usize,
        status: Status,
        out: Option<&Output>,
        attempt: u32,
        attempts: u32,
        duration: Duration,
        detail: Option<String>,
        note: Option<String>,
    ) {
        if !self.mode.reports() {
            return;
        }
        let name = self.step_name(step, scope, &status);
        self.summary.count(&status);
        self.index += 1;
        let ev = Event {
            index: self.index,
            name,
            file: step.at.file.to_path_buf(),
            line: step.at.line(),
            depth,
            status,
            duration,
            attempt,
            attempts,
            stdout: out.map(|o| o.stdout.clone()).unwrap_or_default(),
            stderr: out.map(|o| o.stderr.clone()).unwrap_or_default(),
            detail,
            note,
        };
        self.sink.step(&ev);
    }

    /// Spec §10: a `name` that renders empty falls back to the action key plus
    /// its first argument. A skipped step is not rendered at all, so it uses
    /// the raw name — it may reference a variable that does not exist.
    fn step_name(&mut self, step: &Step<'static>, scope: &Scope, status: &Status) -> String {
        let Some(n) = step.mods.name else {
            return step.fallback_name();
        };
        let Ok(raw) = n.as_str() else {
            return step.fallback_name();
        };
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
        self.engine
            .render(s, ctx)
            .map_err(|e| at.err(self.engine.describe_with(s, ctx, &e)))
    }

    /// Push a file onto the include stack, reporting a cycle by naming it.
    fn enter(&mut self, path: &Path, at: N<'static>) -> Option<()> {
        if let Some(i) = self.stack.iter().position(|p| p == path) {
            let mut chain: Vec<String> = self.stack[i..].iter().map(|p| rel_display(p)).collect();
            chain.push(rel_display(path));
            self.diags
                .push(at.err("import cycle").with_note(chain.join(" → ")));
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

/// What a `register` holds. Spec §4 fixes the shape; with no output it is
/// D14's placeholder — real shape, empty values — so `when: r.changed` reads
/// false and the reader is reported unprobed rather than skipped.
fn result_value(out: Option<&Output>, status: &Status) -> Value {
    let mut m = BTreeMap::new();
    m.insert(
        "rc".to_string(),
        Value::from(out.map(|o| o.rc).unwrap_or(0)),
    );
    m.insert(
        "stdout".to_string(),
        Value::from(out.map(|o| o.stdout.as_str()).unwrap_or("")),
    );
    m.insert(
        "stderr".to_string(),
        Value::from(out.map(|o| o.stderr.as_str()).unwrap_or("")),
    );
    m.insert(
        "changed".to_string(),
        Value::from(matches!(status, Status::Changed)),
    );
    m.insert(
        "skipped".to_string(),
        Value::from(matches!(status, Status::Skipped(_))),
    );
    Value::from(m)
}

/// The runner's three questions, answered from the expander's engine and
/// scope. `result` is bound here and nowhere else — it exists for exactly the
/// lifetime of one expression.
struct StepJudge<'a> {
    engine: &'a Engine,
    base: Map,
    failed_when: Option<String>,
    changed_when: Option<String>,
    at: N<'a>,
}

impl StepJudge<'_> {
    fn ctx(&self, out: &Output) -> Value {
        let mut m = self.base.clone();
        m.insert("result".to_string(), result_value(Some(out), &Status::Ok));
        Value::from(m)
    }

    fn eval(&self, src: &str, ctx: &Value) -> Result<bool> {
        let at = |m: String| self.at.err(m);
        self.engine.eval_bool(src, ctx, &at)
    }
}

impl Judge for StepJudge<'_> {
    fn failed(&self, out: &Output) -> Result<bool> {
        match &self.failed_when {
            Some(src) => self.eval(src, &self.ctx(out)),
            None => Ok(out.rc != 0),
        }
    }

    fn changed(&self, out: &Output) -> Result<Option<bool>> {
        match &self.changed_when {
            Some(src) => self.eval(src, &self.ctx(out)).map(Some),
            None => Ok(None),
        }
    }

    fn expr(&self, src: &str) -> Result<bool> {
        self.eval(src, &Value::from(self.base.clone()))
    }
}

/// A `--prop` value: rendered as a template, then read as the prop's
/// **declared** type (spec §8). A shell has no way to type a value, so the
/// declaration is the only place a type can come from — `--prop count=3` is
/// the int 3 because `count` is declared `int`, and `--prop name=3` is the
/// string "3" because `name` is declared `string`. Deliberately not the
/// sole-expression rule of §3.4: that rule reads the type off the source
/// text, which a command line cannot carry.
fn typed_prop(schema: &load::PropSchema, text: &str) -> std::result::Result<Value, Diag> {
    if schema.ty == load::PropType::String {
        return Ok(Value::from(text));
    }
    let bad = || {
        let msg = format!(
            "prop `{}` is declared {}, and `{text}` does not read as one",
            schema.name,
            schema.ty.name()
        );
        Diag::file_level("--prop", msg).with_note(format!(
            "{} takes {}",
            schema.ty.name(),
            schema.ty.example()
        ))
    };
    // Read through the same YAML the rest of the crate reads, so `3`, `true`
    // and `[a, b]` mean here exactly what they mean in a plan file.
    let value = crate::yaml::Doc::from_str("--prop", text)
        .and_then(|d| d.node().to_value())
        .map_err(|_| bad())?;
    if schema.ty.accepts(&value) {
        Ok(value)
    } else {
        Err(bad())
    }
}

/// The half of prop binding that does not care where the values came from:
/// a declared prop with no value takes its default, fails if it is required,
/// and is null otherwise. `missing_at` places the "missing required" error —
/// at the `use` step for a plan, at `--prop` for the command line.
fn fill_defaults(
    component: &Component,
    out: &mut Map,
    errors: &mut Vec<Diag>,
    missing_at: &dyn Fn(&load::PropSchema) -> Diag,
) {
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
            None if schema.required => errors.push(missing_at(schema).with_note(format!(
                "declared at {}:{}",
                rel_display(&component.path),
                schema.at.line()
            ))),
            None => {
                // Declared, not required, no default: absent is a value.
                out.insert(schema.name.clone(), Value::from(()));
            }
        }
    }
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
