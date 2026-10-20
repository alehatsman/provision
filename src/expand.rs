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
use crate::exec::process::{How, Output};
use crate::exec::runner::{Judge, Prepared, Runner};
use crate::output::Sink;
use crate::report::{Event, Manifest, Status, Summary};
use crate::scope::{Globals, Map, Scope};
use crate::template::{Engine, expanduser_in};
use crate::yaml::Doc;
use crate::yaml::N;
use minijinja::Value;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

/// Spec §4: a step with no `timeout` gets ten minutes.
const DEFAULT_TIMEOUT: Duration = Duration::from_mins(10);

/// The one skip reason whose `name` must not be rendered. Spec §8: a
/// tag-excluded step is filtered *before* anything of it is rendered,
/// precisely so it cannot fail on a variable it was never meant to read — so
/// its name has not been rendered either, and rendering it now would
/// reintroduce exactly the failure the ordering exists to prevent.
///
/// Named rather than spelled twice: `step_name` decides by comparing against
/// this, and a second literal would let the two drift apart silently.
const NOT_SELECTED: &str = "not selected by tags";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    /// Check everything; run nothing; produce no step list.
    Validate,
    /// Check everything and report what would run. `probe` runs the read-only
    /// questions — `unless`, `creates`, `assert` — and is off under
    /// `--plan-no-probe`, where no verdict is claimed at all.
    Plan { probe: bool },
    /// Check everything, then do it.
    Apply,
}

impl Mode {
    fn executes(self) -> bool {
        matches!(self, Mode::Apply)
    }
    fn reports(self) -> bool {
        !matches!(self, Mode::Validate)
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
    /// What the preflight learned about the shape of the run, for the line
    /// `apply` prints before the first step (D25). Counted on the walk that
    /// already visits every step, so it costs nothing and probes nothing.
    pub manifest: Manifest,
    /// Names bound by `register`, and `vars` names computed from one. At plan
    /// time they hold a placeholder, so a template that reads one cannot be
    /// trusted. The step is reported unprobed rather than judged (D14).
    registers: BTreeSet<String>,
    /// Set once a `shell` or `cmd` step has been walked. They are the only
    /// steps whose effects provision cannot describe, so after one of them a
    /// `file` step's missing `src` may be a path the plan itself builds (D18).
    walked_a_command: bool,
    /// See `gate_from_root`.
    gate_from_root: bool,
    /// D18's overturn clause: the paths earlier steps *declared* they would
    /// write — a `git`, `download` or `template` step's `dest`, a `file`
    /// step's `path`. A later `src` inside one of these is pending, not
    /// missing. Unlike `walked_a_command` this is a per-path judgement: a
    /// declaration names one subtree and defers only what is inside it.
    declared: Vec<PathBuf>,
    stack: Vec<PathBuf>,
    /// One entry per enclosing `import`/`use`: the call site's `name:` when
    /// it has one, the file it names otherwise. Pushed and popped with
    /// `stack`, so the two never disagree about where the walk is. This is
    /// what tells two invocations of one component apart (#32).
    call_path: Vec<String>,
    runner: Option<Runner>,
    sink: Box<dyn Sink>,
    index: usize,
    /// Spec §2: apply stops at the first failure. Nothing after it is run,
    /// reported, or rendered.
    stopped: bool,
    /// Spec §8 `--keep-going`: carry on past a failed step, so one run
    /// reports everything broken instead of the first thing.
    keep_going: bool,
    /// D26: on for a plan root under every verb that walks it. Orthogonal to
    /// `Mode`, because the question it answers is about the file and the
    /// three verbs are about how far the walk goes.
    strict: bool,
    /// Spec §8 `--deadline`: when the run's wall clock runs out. `None` is no
    /// deadline, which is the default and every `validate`.
    deadline: Option<Instant>,
    /// Set when a shape-only descent could not be entered — a templated
    /// import path that cannot be rendered without the gated-off scope, or a
    /// file that is not on this machine. The subtree's tags are then unknown,
    /// so the unknown-tag check must not claim a tag is absent from the plan.
    shape_incomplete: bool,
    /// Set while `validate` is inside a subtree its `when` ruled out. Spec
    /// §8: `validate` answers for the file, and a subtree gated off on *this*
    /// machine was simply invisible to it — a `when: os == "linux"`
    /// component carried no weight on a Mac, so the same plan validated
    /// differently on different hosts and an ungated `shell` inside a
    /// cross-platform component was found by whichever machine happened to
    /// match. The walk goes in anyway, but checks **shape only**: nothing is
    /// rendered, because a gated-off step may read variables that do not
    /// exist here and failing on those is precisely the mooncake #191 class
    /// D7's ordering exists to prevent.
    shape_only: bool,
    /// Set while the walk is inside an `import` or `use` whose own `when`
    /// names the os. Spec §3.1: such a `when` "applies to every imported
    /// step", and §6.10's `defaults` gate is the one rule that reads a
    /// step's `when` directly — so without this it read only the step's own
    /// and refused a correctly-gated subtree (#40c).
    os_gated: bool,
    /// Every tag the walk saw declared, on a step or inherited from the
    /// `import`/`use` that carried it. Collected so that a `--tags` value
    /// matching none of them can be named at the end of the walk, and so the
    /// diagnostic can print the set the plan actually has.
    seen_tags: BTreeSet<String>,
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
            manifest: Manifest::default(),
            registers: BTreeSet::new(),
            walked_a_command: false,
            gate_from_root: false,
            declared: Vec::new(),
            stack: Vec::new(),
            call_path: Vec::new(),
            runner: None,
            sink: Box::new(crate::output::Silent),
            index: 0,
            stopped: false,
            keep_going: false,
            strict: false,
            deadline: None,
            shape_incomplete: false,
            shape_only: false,
            os_gated: false,
            seen_tags: BTreeSet::new(),
        }
    }

    /// Spec §8. `apply` only: `plan` and `validate` never stopped at a
    /// failure, so there is nothing for the flag to change there.
    pub(crate) fn keep_going(mut self, yes: bool) -> Expander {
        self.keep_going = yes;
        self
    }

    /// D3, D21, D26: reject a `shell`/`cmd` step with no idempotency gate,
    /// and a `defaults` step that does not say which os it is for. On for a
    /// plan root under every verb that walks it; off for a task or component
    /// root unless `validate --strict` asks.
    pub(crate) fn strict(mut self, on: bool) -> Expander {
        self.strict = on;
        self
    }

    /// Whether the check is on because of the file's *shape* rather than a
    /// flag. Only the diagnostic reads it: the root-shape rule is what makes
    /// a file a plan, and it was stated in no help text and no error, so the
    /// one moment it costs someone something is the one moment to say it.
    pub(crate) fn gate_from_root(mut self, yes: bool) -> Expander {
        self.gate_from_root = yes;
        self
    }

    /// Spec §8 `--deadline`. The clock starts here rather than at
    /// construction, which for `apply` is what keeps the sudo preflight out of
    /// the budget: a password typed at a prompt is the operator's time, not
    /// the job's, and the expander that gets a deadline is built after it.
    pub(crate) fn with_deadline(mut self, d: Option<Duration>) -> Expander {
        self.deadline = d.map(|d| Instant::now() + d);
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

    /// Emitted once before the first step's line.
    pub(crate) fn manifest(&mut self, plan: &Path, m: &Manifest) {
        self.sink.manifest(plan, m);
    }

    /// Emitted once at the end, after the last step's line.
    pub(crate) fn summarize(&mut self, plan: &Path, elapsed: Duration) {
        self.summary.duration = elapsed;
        let s = self.summary.clone();
        self.sink.summary(plan, &s);
    }

    /// The root file, through the cache the walk itself uses. `main` reads
    /// its shape to settle `--strict` (D21, D26) before the walk starts, and
    /// the walk then loads the same path again — so classifying it with a
    /// direct `Doc::load` parsed it twice and leaked the second copy (#62).
    /// Going through the `Loader` makes the second a cache hit.
    pub(crate) fn root_doc(&mut self, root: &Path) -> Result<&'static Doc> {
        self.loader.load(root)
    }

    pub(crate) fn run(&mut self, root: &Path) -> Result<()> {
        let mut scope = Scope::root(Rc::clone(&self.globals));
        let doc = self.loader.load(root)?;
        let (steps, errors) = model::parse_steps(doc.node())?;
        for d in errors {
            self.diags.push(d);
        }
        self.walk(&steps, &mut scope, 0, &BTreeSet::new());
        self.check_selection();
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
        // `Path::new("ci.yml").parent()` is `Some("")`, not `None`, and
        // `std::path::absolute("")` fails -- so `provision apply ci.yml`
        // rendered `component_dir` as the empty string and a preset reaching
        // for `{{ component_dir }}/clippy.toml` got `/clippy.toml`. It errored
        // loudly, but at the wrong path and with no hint that the leading
        // slash was the bug. Same trap as the cwd default before `d531060`:
        // when the file is named bare, its directory *is* the cwd.
        let dir = component
            .path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let mut scope = Scope::root(Rc::clone(&self.globals)).child_with_props(props, dir);
        self.walk(&component.steps, &mut scope, 0, &BTreeSet::new());
        self.check_selection();
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
            self.check_clock();
            // Spec §10: Ctrl-C and SIGTERM stop the run. The handler only
            // records the signal, and the wait on a child is what used to read
            // it — so one that arrived while no child was running, during a
            // `file` write or between two steps, was read by nobody: every
            // remaining step ran and the run exited 0. Every step starts here.
            if crate::exec::process::interrupted() {
                self.summary.interrupted = true;
                self.stopped = true;
            }
            // Spec §8: a problem found while executing ends the walk, the same
            // as a failed step without `--keep-going` — and `--keep-going`
            // does not apply, because a plan that will not render is not a
            // plan to keep going with. The pre-walk finds nearly all of them
            // before step one; what reaches here reads a register, and a
            // `when` or a field that cannot be answered must not let the next
            // step run on its way to exit 3.
            if self.stopped || (self.mode.executes() && !self.diags.is_empty()) {
                return;
            }
            if let Err(d) = model::check_modifiers(step) {
                self.diags.push(d);
                continue;
            }
            self.step(step, scope, depth, inherited);
        }
    }

    /// Time left on the run's clock. `None` is "no deadline", not "no time".
    fn remaining(&self) -> Option<Duration> {
        self.deadline
            .map(|d| d.saturating_duration_since(Instant::now()))
    }

    /// Spec §8: the run's clock, checked before each step and again after it.
    ///
    /// Both, and that is the whole subtlety. Before, so the walk stops without
    /// starting work it cannot finish. After, so a step the deadline itself
    /// killed ends the run on 124 rather than on that step's own 1 — the clock
    /// is why the run ended, and whether `--keep-going` was passed has no
    /// bearing on that.
    ///
    /// Setting `stopped` is what unwinds the walk, so the deadline reuses the
    /// path a first failure already takes rather than inventing a second one.
    fn check_clock(&mut self) {
        if self.deadline.is_some_and(|d| Instant::now() >= d) {
            self.summary.deadline_exceeded = true;
            self.stopped = true;
        }
    }

    /// Spec §8: a `--tags` or `--skip-tags` value that matches no step in the
    /// plan is a usage error, not an empty selection.
    ///
    /// This is a deliberate divergence from D7's "Ansible semantics": Ansible
    /// accepts an unknown tag and runs nothing. Here `apply drift.yml --tags
    /// handlr` reported `2 skipped`, exit 0 — an apply that did nothing
    /// reporting success — and the inverse typo ran everything. Both are
    /// silent, and `break-glass.md` §3b has an operator typing one of these
    /// from memory while something is broken.
    ///
    /// Called at the end of the walk because tags are only known once
    /// `import`/`use` have been expanded (D7's own ordering). Skipped when the
    /// walk already found problems: a plan that failed to load never reached
    /// its tags, and "no step is tagged `x`" would then be a guess.
    ///
    /// **What this asks is whether the tag exists, not whether it selected
    /// anything.** Those are two questions and conflating them was the bug in
    /// the first version: a tag declared nowhere is a typo and is refused on
    /// every host, while a tag that exists and selects nothing *here* — every
    /// `pacman` step gated to Linux, read on a Mac — is the ordinary state of
    /// a cross-platform plan and is not an error. The walk descends into
    /// gated-off subtrees for shape (D27), so the tag set is the file's and
    /// not this machine's.
    fn check_selection(&mut self) {
        // Stand down when the walk did not finish looking. A diagnostic may
        // have aborted it, a deadline or a Ctrl-C may have cut it short
        // (`stopped`), or a shape-only descent may have been abandoned
        // because an import path could not be rendered without the very
        // scope the gate ruled out (`shape_incomplete`). In all three the
        // tag set is partial, and "no step declares `x`" would be a guess —
        // and a false refusal of a tag that is really there is worse than
        // the silence this check replaced.
        if !self.diags.is_empty() || self.mode.executes() || self.stopped || self.shape_incomplete {
            return;
        }
        let known: Vec<&str> = self.seen_tags.iter().map(String::as_str).collect();
        let note = if known.is_empty() {
            "no step in this plan declares any tag".to_string()
        } else {
            format!("tags in this plan: {}", known.join(", "))
        };
        for (flag, values) in [
            ("--tags", &self.selection.tags),
            ("--skip-tags", &self.selection.skip_tags),
        ] {
            for v in values {
                // `always` is exempt on `--skip-tags` only. There it is a
                // documented move that suppresses the keyword's override, and
                // refusing it for lack of an `always`-tagged step would
                // reject the idiom on exactly the plans it is written for.
                //
                // On `--tags` it is not exempt, and granting it the exemption
                // reinstated the bug this check exists to kill: `apply
                // --tags always` on a plan with no `always`-tagged step
                // selected nothing, did nothing, and exited 0 — which is
                // verbatim the "apply that touched nothing reporting success"
                // the D7 amendment quotes as its reason to exist. §8 only
                // ever justified the exemption for `--skip-tags`; the loop
                // applied it to both.
                let exempt = flag == "--skip-tags" && v == "always";
                if exempt || self.seen_tags.contains(v) {
                    continue;
                }
                self.diags.push(
                    Diag::file_level(flag, format!("`{v}` matches no step in this run"))
                        .with_note(note.clone()),
                );
            }
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
                Ok(ts) => tags.extend(ts.iter().map(ToString::to_string)),
                Err(d) => {
                    self.diags.push(d);
                    return;
                }
            }
        }

        // Recorded before the filter, not after: a step the selection excludes
        // still *declares* its tags, and it is exactly the excluded steps that
        // prove a `--tags` value was a real tag rather than a typo.
        self.seen_tags.extend(tags.iter().cloned());

        // Shape-only: inside a subtree `validate` found gated off. No
        // condition is evaluated (it may read variables that do not exist on
        // this machine, which is why the subtree is gated), nothing is
        // rendered, nothing is recorded. The strict gate is the whole point —
        // it is a syntactic question about the step as written, and it is the
        // check that was invisible here.
        if self.shape_only {
            if step.is_structural() {
                let outer = self.os_gated;
                self.os_gated = outer || self.when_names_os(step);
                match step.key {
                    "import" => self.do_import(step, scope, depth, &tags),
                    "use" => self.do_use(step, scope, depth, &tags),
                    _ => {}
                }
                self.os_gated = outer;
            } else if self.strict {
                self.check_strict_gate(step);
            }
            return;
        }

        // Spec §8: tag filtering happens before rendering, so an excluded step
        // cannot fail on a variable it never gets to read. This is mooncake
        // #191 and #196, and it is why the check is here and not below `when`.
        // Structural steps are exempt: they build the scope every later step
        // reads, and selecting work must not silently unset variables.
        if !step.is_structural() && !self.selection.selects(&tags) {
            let status = Status::Skipped(NOT_SELECTED.into());
            self.bind_register(step, scope, None, &status);
            self.record(step, scope, depth, status);
            return;
        }

        match self.condition(step, scope) {
            Cond::False => {
                let status = Status::Skipped("when: false".into());
                self.bind_register(step, scope, None, &status);
                self.record(step, scope, depth, status);
                // Go in anyway, shape only. The step is still reported
                // skipped above — this adds checks, not work or output.
                //
                // The descent itself is unconditional; only the *strict
                // gate* inside it is gated on `strict`. It used to be gated
                // as a whole, which made a tag declared inside a gated-off
                // subtree visible on a plan root and invisible on a task
                // root — so `--tags pacman` was accepted on one and refused
                // on the other for the same bytes. Whether a tag exists is a
                // property of the file (§8); it cannot depend on the root's
                // shape.
                //
                // Nor is it gated on the verb. Strict is a property of the
                // file (D26), so all three verbs that walk a plan root
                // descend here and agree, which is §8's "one file, one set
                // of arguments, one verdict". Restricting this to the
                // `validate` verb would have made it refuse a file `plan`
                // accepts, with the same arguments on the same machine,
                // which is the one thing §8 promises cannot happen. The
                // executing `apply` walk has strict off, so it descends for
                // the tag set and checks nothing; its preflight checks.
                if step.is_structural() {
                    let outer = (self.shape_only, self.os_gated);
                    self.shape_only = true;
                    self.os_gated = outer.1 || self.when_names_os(step);
                    match step.key {
                        "import" => self.do_import(step, scope, depth, &tags),
                        "use" => self.do_use(step, scope, depth, &tags),
                        // `vars`/`vars_file` compute values, and values are
                        // the one thing a false branch has no business
                        // producing: they would land in the live scope and
                        // be read by the steps after it.
                        _ => {}
                    }
                    (self.shape_only, self.os_gated) = outer;
                }
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
                    self.record(step, scope, depth, Status::WouldRunUnprobed);
                    return;
                }
            }
            Cond::True => {}
        }

        // Saved and restored rather than set: an os-gated `import` gates the
        // steps *inside* it, and the sibling that follows it is gated by
        // nothing. Only structural steps can carry a gate down, and only an
        // `import`/`use` descends.
        let outer = self.os_gated;
        if step.is_structural() {
            self.os_gated = outer || self.when_names_os(step);
        }
        match step.key {
            "vars" => self.do_vars(step, scope),
            "vars_file" => self.do_vars_file(step, scope),
            "import" => self.do_import(step, scope, depth, &tags),
            "use" => self.do_use(step, scope, depth, &tags),
            _ => self.do_action(step, scope, depth),
        }
        self.os_gated = outer;
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
        if self.reads_a_register(expr, scope) {
            return Cond::Unprobed;
        }
        let at = |m: String| when.err(m);
        match self.engine.eval_bool(expr, &scope.ctx(), &at) {
            Ok(true) => Cond::True,
            Ok(false) => Cond::False,
            Err(d) => Cond::Error(d),
        }
    }

    fn reads_a_register(&self, expr: &str, scope: &Scope) -> bool {
        // At apply time the registering step has actually run, so the value is
        // the real one and the condition is answerable. This guard is the
        // whole difference between D14's placeholder and a real result.
        if self.mode.executes() || self.registers.is_empty() {
            return false;
        }
        // Top-level names only: `gitcfg.changed` reads the register `gitcfg`.
        //
        // And only a register this scope can see. `self.registers` holds every
        // name registered anywhere in the run, but a component's register is
        // bound in the component's scope and does not leak back (§3.1). A
        // parent reading one used to count as reading a result not yet run,
        // so `validate` and `plan` skipped it as unprobed and only `apply`
        // found the name undefined. Every registering step binds its name —
        // a placeholder when nothing ran — so a register in reach is defined.
        self.any_register(self.engine.undeclared(expr), scope)
    }

    /// `reads_a_register` for a template string: a field, a `name`, a `vars`
    /// value (D14, amended for #16).
    fn template_reads_a_register(&self, src: &str, scope: &Scope) -> bool {
        if self.mode.executes() || self.registers.is_empty() {
            return false;
        }
        self.any_register(self.engine.undeclared_in_template(src), scope)
    }

    fn node_reads_a_register(&self, n: N<'static>, scope: &Scope) -> bool {
        let mut strings = Vec::new();
        collect_strings(n, &mut strings);
        strings.iter().any(|s| {
            s.as_str()
                .is_ok_and(|src| self.template_reads_a_register(src, scope))
        })
    }

    fn any_register(&self, names: Option<HashSet<String>>, scope: &Scope) -> bool {
        names.is_some_and(|names| {
            names
                .iter()
                .any(|n| self.registers.contains(n) && scope.defines(n))
        })
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
            // A value computed from a register is register-derived itself
            // (D14, #16). Under `validate` and `plan` it was computed from a
            // placeholder, so a filter that needs real output may fail on it.
            // That is not a problem, and its readers are unprobed.
            let derived = self.node_reads_a_register(v, scope);
            match self.engine.render_node(v, &ctx) {
                Ok(value) => scope.set(name.clone(), value),
                Err(_) if derived => scope.set(name.clone(), Value::from("")),
                Err(d) => self.diags.push(d),
            }
            if derived {
                self.registers.insert(name);
            }
        }
    }

    fn do_vars_file(&mut self, step: &Step<'static>, scope: &mut Scope) {
        let optional = step
            .mods
            .optional
            .is_some_and(|n| n.as_bool().unwrap_or(false));
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
            #[expect(
                clippy::map_err_ignore,
                reason = "the replacement diagnostic restates the cause"
            )]
            let mapped = root.as_map().map_err(|_| {
                entry.err(format!(
                    "vars_file {} is {}, expected a mapping of variables",
                    path.display(),
                    root.kind()
                ))
            });
            let Some(pairs) = self.diags.absorb(mapped) else {
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
        let Some(doc) = self.open(step, scope, "import") else {
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
        self.leave();
    }

    fn do_use(
        &mut self,
        step: &Step<'static>,
        scope: &mut Scope,
        depth: usize,
        tags: &BTreeSet<String>,
    ) {
        let Some(doc) = self.open(step, scope, "use") else {
            return;
        };
        let Some(component) = self.diags.absorb(load::parse_component(doc)) else {
            self.leave();
            return;
        };

        // D27: inside a subtree this machine gates off, the descent is shape
        // only. `bind_props` renders every prop value against the live scope
        // and type-checks the result, which is exactly what must not happen
        // here — a gated-off `use:` legitimately passes
        // `props: {session: "{{ desktop_session }}"}` from an os-gated
        // `vars_file` that was never loaded. `open()` above was already
        // shape-aware and this was not, so the two disagreed one line apart.
        //
        // Worse than a spurious error: `bind_props` returning `Err` means the
        // component's steps are never walked, so the strict gate never runs
        // on them. A templated prop silently disabled the very check D27
        // exists to perform. Walk with empty props instead — nothing inside
        // is rendered under `shape_only`, so nothing reads them.
        if self.shape_only {
            let dir = component.path.parent().unwrap_or(Path::new("."));
            let mut child = scope.child_with_props(Map::new(), dir);
            self.walk(&component.steps, &mut child, depth + 1, tags);
            self.leave();
            return;
        }

        match self.bind_props(step, &component, scope) {
            Ok(props) => {
                // `use` runs in a child scope: the parent is visible read-only
                // and the component's own vars do not leak back (spec §3.1).
                // A `use` path is always resolved against the naming file,
                // so it always has a parent.
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
        self.leave();
    }

    /// Find, enter and load the file an `import` or `use` names. `Some` means
    /// it is on the include stack and the caller pops it when done; `None`
    /// has reported why and left the stack as it was.
    fn open(&mut self, step: &Step<'static>, scope: &Scope, key: &str) -> Option<&'static Doc> {
        let ctx = scope.ctx();
        // The one thing a shape-only walk renders is the path of the next
        // file, because there is no other way to reach it. It is also the one
        // thing that can fail for a reason the walk must not report: a gated
        // -off subtree may name its imports through variables that do not
        // exist on this machine -- `import: "{{ linux_root }}/more.yml"` --
        // and that file is not expected to be here either. Either failure
        // just ends the descent. This is mooncake #191 in its last hiding
        // place: the subtree is skipped, so it must not fail on what it reads.
        let rendered = self.render_str(step.body, &ctx);
        let rel = if self.shape_only {
            let Ok(r) = rendered else {
                self.shape_incomplete = true;
                return None;
            };
            r
        } else {
            self.diags.absorb(rendered)?
        };
        let path = load::resolve(step.body.file, &rel);
        if !path.exists() {
            if self.shape_only {
                self.shape_incomplete = true;
            } else {
                self.diags.push(load::missing(step.body, &path, key));
            }
            return None;
        }
        // The call site's own name is the label, and the file it names is
        // the fallback — which is what the header has always shown, so an
        // unnamed `use` reads exactly as it did. Rendered leniently: a name
        // that will not render is reported by the field check, and a
        // shape-only walk must not report anything at all.
        let label = step.mods.name.and_then(|n| n.as_str().ok()).map_or_else(
            || rel_display(&path),
            |raw| match self.engine.render(raw, &ctx) {
                Ok(rendered) if !rendered.trim().is_empty() => rendered,
                _ => raw.to_string(),
            },
        );
        self.enter(&path, step.body, label)?;
        let doc = self.diags.absorb(self.loader.load(&path));
        if doc.is_none() {
            self.leave();
        }
        doc
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
        // Props whose value was given and rejected. `fill_defaults` would
        // otherwise see the name absent from `out`, find it required, and
        // add a second diagnostic reading "missing required prop" that
        // points at the component's declaration -- which is not a second
        // problem, it is the first one told backwards, sending the reader to
        // the wrong file.
        let mut errored: HashSet<String> = HashSet::new();
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
                        errored.insert(name);
                        continue;
                    }
                    out.insert(name, value);
                }
                Err(d) => {
                    errors.push(d);
                    errored.insert(name);
                }
            }
        }

        fill_defaults(component, &mut out, &errored, &mut errors, &|schema| {
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
        // See `bind_props`: a prop that was given and rejected is not also
        // missing.
        let mut errored: HashSet<String> = HashSet::new();
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
                    Err(d) => {
                        errors.push(d);
                        errored.insert(name.clone());
                    }
                },
                // A `--prop` value is a template (§8), and this threw away
                // the context the way `eval_bool` used to, so an undefined
                // name in one had no name in the error.
                Err(e) => {
                    errors.push(Diag::file_level(
                        "--prop",
                        self.engine.describe_with(raw, &ctx, &e),
                    ));
                    errored.insert(name.clone());
                }
            }
        }

        fill_defaults(component, &mut out, &errored, &mut errors, &|schema| {
            Diag::file_level("--prop", format!("missing required prop `{}`", schema.name))
        });

        if errors.is_empty() {
            Ok(out)
        } else {
            Err(errors)
        }
    }

    // ── actions ───────────────────────────────────────────────────────────

    /// Takes no tags: filtering already happened in `step`, and the two
    /// reporting helpers below stopped reading them when `record` and `emit`
    /// did.
    fn do_action(&mut self, step: &Step<'static>, scope: &mut Scope, depth: usize) {
        let ctx = scope.ctx();

        // Render every string field. This is where an undefined variable in a
        // rarely-taken branch surfaces, which is the point of strict mode. It
        // is also the only place that reports render errors: `prepare` below
        // renders the same nodes again and stays quiet, so one bad `{{ … }}`
        // is one diagnostic rather than two.
        let raw = step.mods.raw.is_some_and(|n| n.as_bool().unwrap_or(false));
        let mut fields: Vec<N<'static>> = Vec::new();
        if !raw {
            collect_strings(step.body, &mut fields);
        }
        // `name` too: `plan` and `apply` render it for the step's line, and an
        // undefined variable there failed both while `validate` said ok —
        // spec §8's "anything plan would reject, validate rejects too".
        for n in [
            step.mods.name,
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
        // A field that reads a register is rendered from the placeholder
        // under `validate` and `plan` (D14, #16). Its render can fail on a
        // value `apply` never sees, so that failure is not a diagnostic, and
        // the step as a whole cannot be judged.
        let mut renderable = true;
        let mut derived = false;
        for f in fields {
            let reads = f
                .as_str()
                .is_ok_and(|src| self.template_reads_a_register(src, scope));
            derived |= reads;
            if let Err(d) = self.render_str(f, &ctx) {
                if !reads {
                    self.diags.push(d);
                }
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
                    if let Err(d) = self.engine.check_expression(&src, &at) {
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
                .deny_unknown_keys(&allowed, &format!("`{}`", step.key))
        {
            self.diags.push(d);
            renderable = false;
        }

        // §3.2: a prop `default` is taken as written, so a `{{` can survive
        // rendering. A string may hold one; a path may not — and neither may
        // hold a `$VAR`, which provision never expands.
        self.check_rendered_path(step, &ctx, raw);

        // A source path rendered from a placeholder names a file that apply
        // alone knows.
        if step.key == "template" && !derived {
            derived = self.check_template_sources(step, &ctx, scope);
        }
        if matches!(step.key, "file" | "json") && !derived && !self.check_file_source(step, &ctx) {
            // The source is named and is not there. `prepare` would reach the
            // same file and say `cannot read … No such file`, one fault
            // reported twice in two wordings — and the one above is the one
            // that carries the note about how paths resolve (§9.3).
            renderable = false;
        }
        // Set after this step's own check: the rule reads "earlier in the
        // walk", and a step is not earlier than itself. Same for the path
        // this step declares — a `file` step's `src` under its own `path` is
        // still missing.
        if matches!(step.key, "shell" | "cmd") {
            self.walked_a_command = true;
        }
        let defer_src = self.defers_file_src(step, &ctx);
        if let Some(out) = self.declared_output(step, &ctx) {
            self.declared.push(out);
        }

        // The three typed modifiers §3.4's rule now covers are checked here,
        // with the scope, because parsing could not check them: a template
        // has no value until there is one (#35). Every read of them below is
        // deliberately lenient -- `.ok()`, `unwrap_or` -- which was safe while
        // parsing had already proved the type, and would silently turn a bad
        // `sudo: "{{ s }}"` into `false` without this.
        // `sudo` and `timeout` only when they hold a template -- a literal was
        // already proved at parse time and re-reading it buys nothing.
        // `retry` always, because it is a mapping whose two fields are checked
        // individually and either may be a template.
        //
        // First, though: a typed modifier that reads a register is refused
        // outright, not rendered. D14's placeholder makes the preflight render
        // a value the real walk will not see, and these three are consumed in
        // places where a wrong value is not a wrong *verdict* but a wrong
        // *run*:
        //
        //   `sudo`    decides whether the step escalates, and §7 promises the
        //             preflight finds every sudo step so the password is asked
        //             for once, before anything happens. Rendered from a
        //             placeholder it came out `false`, the manifest counted
        //             `sudo: 0`, `Sudo::preflight` returned without proving
        //             root, and the step then ran unprivileged. Privilege is a
        //             precondition and part of the shape of the run, like a
        //             `use:` path (§3.2) -- not something a run discovers
        //             about itself halfway through.
        //
        //   `timeout` feeds §8's deadline arithmetic, which is done in the
        //   `retry`   preflight too, and both are read leniently at execution,
        //             so a value that failed to resolve became the 10m default
        //             and no-retry with nothing said.
        //
        // The fix is the same one D27 and #27 already chose for the other
        // undecidable-before-the-run questions: refuse, rather than defer. A
        // step whose privilege depends on an earlier step's output is two
        // plans, and says so.
        for (node, name) in [
            (step.mods.sudo, "sudo"),
            (step.mods.timeout, "timeout"),
            (step.mods.retry, "retry"),
        ] {
            let Some(n) = node else { continue };
            if self.node_reads_a_register(n, scope) {
                self.diags.push(
                    n.err(format!("`{name}` cannot read a register"))
                        .with_note(match name {
                            "sudo" => "whether a step needs root must be known before the run starts (§7); gate the step with `when:` instead, or split the plan",
                            _ => "it is read before the first step runs, so a value that does not exist yet would silently become the default",
                        }),
                );
                return;
            }
        }

        let checks = [
            step.mods
                .sudo
                .filter(|n| model::is_template(*n))
                .map(|n| model::mod_bool(n, &self.engine, &ctx).map(|_| ())),
            step.mods
                .timeout
                .filter(|n| model::is_template(*n))
                .map(|n| model::mod_duration(n, &self.engine, &ctx).map(|_| ())),
            step.mods
                .retry
                .map(|n| model::mod_retry(n, &self.engine, &ctx).map(|_| ())),
        ];
        for d in checks.into_iter().flatten().filter_map(Result::err) {
            self.diags.push(d);
        }

        // Rendered, so a templated `sudo` is counted for what it resolves to
        // and not skipped for not being a literal (#35). A sudo that cannot
        // be read at all counts as root: spec §7's preflight over-approximates
        // on purpose — asking for a password the run turns out not to need is
        // recoverable, discovering root is needed halfway through is not.
        if step
            .mods
            .sudo
            .is_some_and(|n| model::mod_bool(n, &self.engine, &ctx).unwrap_or(true))
        {
            self.needs_sudo = true;
            self.manifest.sudo += 1;
        }
        self.manifest.steps += 1;
        self.manifest.files.insert(step.at.file.to_path_buf());

        // D26: strict is a property of the file, not of the verb. Checked
        // here, after `needs_sudo` and `walked_a_command` are set and after
        // the `when: false` return above, because this position is what gives
        // a gated-off step its exemption.
        if self.strict {
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
            // Not for a derived step: its body holds placeholders, and a
            // typed field such as a `mode` would be judged on a value apply
            // never sees.
            if renderable
                && !derived
                && let Err(d) = Action::parse(&self.engine, step, &ctx, raw, defer_src)
            {
                self.diags.push(d);
            }
            // `validate` binds the placeholder so a later `when` that reads
            // this register still compiles. Nothing runs.
            self.bind_register(step, scope, None, &Status::WouldRunUnprobed);
            return;
        }

        // A step rendered from placeholders is not probed: a gate rendered
        // from one answers for the wrong value (D14).
        if derived {
            self.bind_register(step, scope, None, &Status::WouldRunUnprobed);
            self.record(step, scope, depth, Status::WouldRunUnprobed);
            return;
        }

        // A step whose fields would not render cannot be run or judged, and
        // the diagnostic already says why.
        let prepared = if renderable {
            self.prepare(step, &ctx, raw, defer_src)
        } else {
            None
        };
        let Some(prepared) = prepared else {
            self.bind_register(step, scope, None, &Status::WouldRunUnprobed);
            self.record(step, scope, depth, Status::WouldRunUnprobed);
            return;
        };

        let unprobed = matches!(self.mode, Mode::Plan { probe: false });
        if unprobed {
            self.bind_register(step, scope, None, &Status::WouldRunUnprobed);
            self.record(step, scope, depth, Status::WouldRunUnprobed);
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
        //
        // Nor past a failed `assert`. The flag exists so one run reports
        // everything broken, which holds because failed *work* is
        // independent -- one package that would not install says nothing
        // about the next. A failed assert is not work: it is a claim about
        // the machine that turned out false, and every step after it would
        // run on premises the plan has already said it does not understand.
        // This is load-bearing rather than tidy: D25's answer to pointing
        // `apply` at the wrong machine is a host guard written as an `assert`
        // tagged `always`, and `--keep-going` disarmed it -- the guard failed
        // and the work it guards ran anyway.
        let premise_failed = done.status.failed() && step.key == "assert";
        if done.status.failed()
            && self.mode.executes()
            && (!self.keep_going || done.interrupted() || premise_failed)
        {
            self.stopped = true;
        }
        self.emit(step, scope, depth, done, elapsed);
        // The other half of the rule in `check_clock`: a step killed by the
        // run's clock has to end the run on the clock's exit code, and the
        // failure it reported cannot say that by itself.
        self.check_clock();
    }

    /// Turn a checked step into something the runner can execute.
    ///
    /// Renders are silent here on purpose: every node this touches was already
    /// rendered once above, and anything that failed was reported there.
    fn prepare(
        &mut self,
        step: &Step<'static>,
        ctx: &Value,
        raw: bool,
        defer_src: bool,
    ) -> Option<Prepared> {
        let action = match Action::parse(&self.engine, step, ctx, raw, defer_src) {
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

        // Spec §4, phase 5b: `cwd` defaults to the invocation directory, for
        // every command and every step, including one inside a `use`d
        // component. A command runs where the operator is standing; a shared
        // gate checked out under ~/.cache must gate the repo you are standing
        // in and not `cd` to its own toplevel to gate itself. `None` is
        // exactly "inherit the process cwd", so the default computes nothing.
        //
        // This used to be the step's own file's directory, which made the
        // same component mean two things depending on the verb that entered
        // it. Measured against the fleet before it went: no step depended on
        // the old default — zero `cwd:` modifiers, zero relative paths in
        // shell strings — and one rule beats two.
        //
        // An explicit `cwd:` still resolves against the step's own file, like
        // every other path a plan names.
        // `~` here is the machine the command runs on, which is the one the
        // scope's `home` names (spec §3).
        let cwd =
            text(step.mods.cwd).map(|dir| load::resolve(step.at.file, &expanduser_in(ctx, &dir)));

        // Spec §8: a step's effective timeout is the lesser of its own and the
        // time the run has left, so no step can outlive the deadline it was
        // admitted under. Without the clamp, `--deadline 30s` on a plan whose
        // next step takes the ten-minute default is a thirty-second promise
        // and a ten-minute run.
        let own_timeout = step
            .mods
            .timeout
            .and_then(|n| model::mod_duration(n, &self.engine, ctx).ok())
            .unwrap_or(DEFAULT_TIMEOUT);
        let left = self.remaining();
        let timeout = left.map_or(own_timeout, |l| l.min(own_timeout));

        Some(Prepared {
            action,
            unless: text(step.mods.unless),
            creates: text(step.mods.creates).map(|path| {
                load::resolve(step.at.file, &path)
                    .to_string_lossy()
                    .into_owned()
            }),
            cwd,
            env,
            sudo: step
                .mods
                .sudo
                .is_some_and(|n| model::mod_bool(n, &self.engine, ctx).unwrap_or(false)),
            timeout,
            retry: step
                .mods
                .retry
                .and_then(|n| model::mod_retry(n, &self.engine, ctx).ok()),
            has_changed_when: step.mods.changed_when.is_some(),
            deadline_bound: timeout < own_timeout,
            own_timeout,
            deadline: self.deadline,
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
    ///
    /// Spec §6.10 adds one more: a `defaults` step with no `when` naming the
    /// os. The action does not skip itself on Linux -- it fails -- so a plan
    /// that carries one has to say where it runs. An error rather than a
    /// warning because strict has one severity, and a second one is a bigger
    /// idea than this rule pays for.
    /// Does this step's own `when` read the `os` fact? A name the expression
    /// reads, not a substring of its text: `hostname == "x1"` contains "os"
    /// and says nothing about the os.
    fn when_names_os(&self, step: &Step<'static>) -> bool {
        step.mods
            .when
            .and_then(|n| n.as_str().ok())
            .and_then(|w| self.engine.undeclared_nested(w))
            // `facts.os` as well as `os`. The namespace is the *unshadowable*
            // spelling (§5) and `examples/x1.yml` uses it for the host guard,
            // so refusing it here demanded the weaker form — the tool telling
            // an author to write the one a `vars` step can defeat.
            .is_some_and(|names| names.iter().any(|n| n == "os" || n == "facts.os"))
    }

    fn check_strict_gate(&mut self, step: &Step<'static>) {
        if step.key == "defaults" {
            // Spec §3.1: a `when` on the `import`/`use` that brought this
            // step in applies to it as surely as one written on the step, so
            // an os-gated component is gated whichever of the two carries the
            // gate. Reading only the step's own `when` refused the shape the
            // spec recommends — a macOS component imported under
            // `when: os == "darwin"` — while accepting the same `when`
            // pasted onto each step inside it (#40c).
            if !(self.os_gated || self.when_names_os(step)) {
                self.diags.push(
                    step.at
                        .err("`defaults` step has no `when` naming the os")
                        .with_note("`when: os == \"darwin\"` — it fails on every other os"),
                );
            }
            return;
        }
        if !matches!(step.key, "shell" | "cmd") {
            return;
        }
        let gated = step.mods.unless.is_some()
            || step.mods.creates.is_some()
            || step.mods.changed_when.is_some();
        if !gated {
            let mut note = "add `unless:`, `creates:`, or `changed_when:`".to_string();
            if self.gate_from_root {
                note.push_str(
                    " — this file's root is a list, which makes it a convergence plan and \
                     holds it to the gate; a `description:`/`steps:` root is a task and is not",
                );
            }
            self.diags.push(
                step.at
                    .err(format!("`{}` step has no idempotency gate", step.key))
                    .with_note(note),
            );
        }
    }

    /// Render the `.j2` sources a `template` step names, so an undefined
    /// variable inside a template is an error at plan time, not at apply time.
    /// Directory mode (spec §6.4) renders every file under `src`.
    /// Returns whether any source body reads a register (D14). The step's
    /// *fields* are tested by the caller; a `.j2` file's *bytes* are only
    /// readable here, and until this returned something the boundary between
    /// the two was the boundary of D14's rule — `content: "{{ fp.stdout }}"`
    /// was correctly `unprobed` while the identical expression in a `.j2`
    /// body produced a `would change` with the placeholder's empty string
    /// baked into the diff. A fabricated diff is worse than no verdict: it is
    /// plan claiming to know a thing it invented.
    fn check_template_sources(&mut self, step: &Step<'static>, ctx: &Value, scope: &Scope) -> bool {
        let Some(src_at) = step.body.get("src") else {
            self.diags.push(step.body.err("`template` requires `src`"));
            return false;
        };
        let Some(rel) = self.diags.absorb(self.render_str(src_at, ctx)) else {
            return false;
        };
        let path = load::resolve(src_at.file, &rel);
        if !path.exists() {
            self.diags
                .push(load::missing(src_at, &path, "template src"));
            return false;
        }
        let files = if path.is_dir() {
            match collect_tree(&path) {
                Ok(f) => f,
                Err(e) => {
                    self.diags
                        .push(src_at.err(format!("cannot walk {}: {e}", path.display())));
                    return false;
                }
            }
        } else {
            vec![path]
        };
        let mut derived = false;
        for f in files {
            let Ok(text) = std::fs::read_to_string(&f) else {
                continue; // a binary asset in the tree is copied, not rendered
            };
            // Same rule as a step field: a body rendered from the placeholder
            // is not the body apply will write, so the step has no verdict
            // and a failure to render it is not a diagnostic either.
            if self.template_reads_a_register(&text, scope) {
                derived = true;
                continue;
            }
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
        derived
    }

    /// D18: whether a `file` step's missing `src` is unresolved rather than
    /// wrong. The walk that executes reaches the step after the earlier steps
    /// have run, so it never defers and a missing source is still an error
    /// there. On a walk that runs nothing there are two reasons to defer, and
    /// they differ in how much they cover:
    ///
    /// * a `shell` or `cmd` step has been walked — it declares nothing, so it
    ///   defers every later `src`;
    /// * this `src` is at or under a path an earlier step declared it would
    ///   write — that names one subtree, so it defers only what is inside it.
    fn defers_file_src(&self, step: &Step<'static>, ctx: &Value) -> bool {
        if self.mode.executes() {
            return false;
        }
        if self.walked_a_command {
            return true;
        }
        let Some(path) = self.resolved_path(step, "src", ctx) else {
            return false;
        };
        self.declared.iter().any(|d| path.starts_with(d))
    }

    /// The path a step's `key` field names, rendered and resolved against the
    /// file that wrote it. Silent: every one of these fields was already
    /// rendered by the field check, so a failure here has been reported.
    fn resolved_path(&self, step: &Step<'static>, key: &str, ctx: &Value) -> Option<PathBuf> {
        let at = step.body.get(key)?;
        let rel = self.render_str(at, ctx).ok()?;
        Some(load::resolve(at.file, &crate::template::expanduser(&rel)))
    }

    /// D18's overturn clause: the path this step says it will write, if it
    /// says. `file: {state: absent}` declares a removal, which is not an
    /// output — and `shell` and `cmd` declare nothing at all, which is the
    /// whole reason they defer everything instead.
    fn declared_output(&self, step: &Step<'static>, ctx: &Value) -> Option<PathBuf> {
        let key = match model::action_doc(step.key)?.dest {
            model::Dest::None => return None,
            model::Dest::Path(key) => key,
            model::Dest::PathUnlessAbsent(key) => {
                let absent = step
                    .body
                    .get("state")
                    .and_then(|s| s.as_str().ok())
                    .is_some_and(|s| s == "absent");
                if absent {
                    return None;
                }
                key
            }
        };
        self.resolved_path(step, key, ctx)
    }

    /// The paths provision reads **itself**, rather than handing to a shell.
    /// The same set `expanduser_in` calls "a location on the target", plus
    /// `creates`, which provision stats. `unless` is deliberately absent: it
    /// is a shell command, where `$VAR` expands and is the right way to
    /// write it.
    ///
    /// Two things cannot survive rendering in one of these:
    ///
    /// * `{{` — §3.2 takes a prop's `default` as written, never rendered, so
    ///   a default holding `{{ home }}/x` arrives as those characters. Legal
    ///   for a string, never meant for a path: `apply` would create a
    ///   directory named `{{ home }}`.
    /// * `$NAME` — shell syntax in a field no shell sees, so it is a literal
    ///   path with a `$` in it. Correct by the rule and almost never meant:
    ///   a `creates: $CACHE_DIR/hit` never matches, so the gate degrades to
    ///   "always run" while `--strict` stays satisfied, because a `creates`
    ///   is present.
    ///
    /// Both used to pass `validate` and `plan` in silence and show up only at
    /// the verb that touches the machine, which inverts what the two
    /// read-only verbs are for.
    fn check_rendered_path(&mut self, step: &Step<'static>, ctx: &Value, raw: bool) {
        // A `raw` body is not rendered, so braces in it are the author's.
        let body_key = if raw {
            None
        } else {
            model::action_doc(step.key).and_then(|d| d.dest.field())
        };
        let body_keys: &[&str] = body_key.as_slice();
        let fields = body_keys
            .iter()
            .map(|k| ((*k).to_string(), step.body.get(k)))
            // Modifiers are rendered whatever `raw` says, so they are checked
            // whatever `raw` says.
            .chain([
                ("creates".to_string(), step.mods.creates),
                ("cwd".to_string(), step.mods.cwd),
            ]);
        for (key, node) in fields {
            let Some(at) = node else {
                continue;
            };
            let Ok(rendered) = self.render_str(at, ctx) else {
                continue; // the field check already reported why
            };
            if rendered.contains("{{") {
                self.diags.push(
                    at.err(format!("`{key}` still contains `{{{{` after rendering"))
                        .with_note(
                            "a prop `default` is taken as written, never rendered — pass the \
                             value at the call site, or compute it in a `vars` step",
                        ),
                );
            }
            if let Some(name) = shell_var(&rendered) {
                self.diags.push(
                    at.err(format!("`{key}` holds the shell variable `${name}`"))
                        .with_note(format!(
                            "provision renders this path itself and no shell sees it, so \
                             `${name}` is a literal — write `{{{{ env.{name} }}}}`"
                        )),
                );
            }
        }
    }

    /// Spec §3.1: a `use` child sees the caller, and the caller does not see
    /// the child — a component's own `vars` and `register` bindings do not
    /// leak back. That boundary explained itself nowhere at the point it
    /// cost anyone anything: reading a value registered inside a component
    /// gave a bare ``undefined variable `version` `` with no hint that the
    /// name exists one scope down and deliberately does not reach here.
    ///
    /// Run once, after the walk: the set of every register bound anywhere is
    /// not complete until then.
    pub(crate) fn explain_scoped_registers(&mut self) {
        if self.registers.is_empty() {
            return;
        }
        self.diags.annotate(|d| {
            let name = d
                .msg
                .strip_prefix("undefined variable `")?
                .strip_suffix('`')?;
            if !self.registers.contains(name) {
                return None;
            }
            Some(format!(
                "`{name}` is registered inside a `use`, and a component's own vars and \
                 registers do not leak back to its caller — register it in this file, or \
                 pass the value down as a prop"
            ))
        });
    }

    /// Spec §8: `src` is one of the paths existence is checked for before
    /// anything runs — unless the plan itself is what puts it there (D18).
    ///
    /// Returns whether the step can still be prepared. A `src` that is named
    /// and absent cannot, and saying so here is what keeps the fault to one
    /// diagnostic.
    fn check_file_source(&mut self, step: &Step<'static>, ctx: &Value) -> bool {
        if self.defers_file_src(step, ctx) {
            return true;
        }
        let Some(src_at) = step.body.get("src") else {
            return true;
        };
        let Some(rel) = self.diags.absorb(self.render_str(src_at, ctx)) else {
            return false;
        };
        // A `link` target need not exist on this machine.
        let is_link = step
            .body
            .get("state")
            .and_then(|s| s.as_str().ok())
            .is_some_and(|s| s == "link");
        if is_link {
            return true;
        }
        let path = load::resolve(src_at.file, &rel);
        if !path.exists() {
            // The label names the action the reader wrote, not the one whose
            // rule this is: `json` shares `file`'s `src` semantics (§6.11),
            // and a `json` step told "file src not found" sends them looking
            // for a `file` step they never wrote.
            self.diags
                .push(load::missing(src_at, &path, &format!("{} src", step.key)));
            return false;
        }
        true
    }

    // ── helpers ───────────────────────────────────────────────────────────

    /// A step that reached a verdict without running: skipped, or unprobed.
    ///
    /// Takes no tags, unlike `emit`: a step that never ran has nothing to
    /// report about them, and the parameter was being discarded on the first
    /// line.
    fn record(&mut self, step: &Step<'static>, scope: &Scope, depth: usize, status: Status) {
        self.finish(
            step,
            scope,
            depth,
            status,
            None,
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
        done: crate::exec::runner::Done,
        elapsed: Duration,
    ) {
        let (detail, note) = (done.detail.clone(), done.note.clone());
        self.finish(
            step,
            scope,
            depth,
            done.status,
            done.out.as_ref(),
            done.rc,
            done.attempt,
            done.attempts,
            elapsed,
            detail,
            note,
        );
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "one walk step: the step, its scope, its depth, its tags and the runner's answer"
    )]
    fn finish(
        &mut self,
        step: &Step<'static>,
        scope: &Scope,
        depth: usize,
        status: Status,
        out: Option<&Output>,
        rc: Option<i32>,
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
            rc,
            detail,
            note,
            via: self.call_path.clone(),
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
        // A tag exclusion keeps the raw name (see `NOT_SELECTED`). Every other
        // skip — `when`, `unless`, `creates` — has already rendered its gate
        // against this scope, so the name renders against the same scope with
        // nothing new to fail on, and printing `Pin rust-quality to
        // {{ rq_version }}` when the same step failing prints the value was
        // just the reader losing information for no reason.
        if matches!(status, Status::Skipped(why) if why == NOT_SELECTED) {
            return raw.to_string();
        }
        // A skipped step's name is not worth a diagnostic: nothing ran, and
        // the raw text still identifies the line. A step that *did* run and
        // whose name would not render is a real fault in the plan and keeps
        // its error.
        if matches!(status, Status::Skipped(_)) {
            return match self.engine.render(raw, &scope.ctx()) {
                Ok(s) if !s.trim().is_empty() => s,
                _ => raw.to_string(),
            };
        }
        match self.engine.render(raw, &scope.ctx()) {
            Ok(s) if !s.trim().is_empty() => s,
            Ok(_) => step.fallback_name(),
            // Rendered from a placeholder, and `do_action` already let it
            // pass (D14, #16). The raw text identifies the line.
            Err(_) if self.template_reads_a_register(raw, scope) => raw.to_string(),
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
    fn enter(&mut self, path: &Path, at: N<'static>, label: String) -> Option<()> {
        if let Some(i) = self.stack.iter().position(|p| p == path) {
            let mut chain: Vec<String> =
                self.stack.iter().skip(i).map(|p| rel_display(p)).collect();
            chain.push(rel_display(path));
            self.diags
                .push(at.err("import cycle").with_note(chain.join(" → ")));
            return None;
        }
        self.stack.push(path.to_path_buf());
        self.call_path.push(label);
        Some(())
    }

    /// The other half of `enter`. Both vectors pop together or neither does.
    fn leave(&mut self) {
        self.stack.pop();
        self.call_path.pop();
    }
}

enum Cond {
    True,
    False,
    Unprobed,
    Error(Diag),
}

/// The first `$NAME` or `${NAME}` in a rendered path, if there is one.
/// Deliberately not a general "contains a `$`" test: a lone `$`, and a `$`
/// followed by anything that is not an identifier, is just a character in a
/// filename.
fn shell_var(s: &str) -> Option<String> {
    // Byte-wise: every character that can be part of one of these — `$`,
    // `{`, `}`, and an identifier — is ASCII, so a multi-byte character can
    // only ever end a name, never split one.
    let b = s.as_bytes();
    for (i, _) in s.match_indices('$') {
        let mut j = i + 1;
        let braced = b.get(j) == Some(&b'{');
        if braced {
            j += 1;
        }
        let start = j;
        while b
            .get(j)
            .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_')
        {
            j += 1;
        }
        let closed = !braced || b.get(j) == Some(&b'}');
        // A name cannot start with a digit: `$1` is a shell positional, not
        // something `{{ env.… }}` could ever have been.
        let first_is_digit = b.get(start).is_some_and(u8::is_ascii_digit);
        if closed
            && j > start
            && !first_is_digit
            && let Some(name) = b.get(start..j)
        {
            return Some(String::from_utf8_lossy(name).into_owned());
        }
    }
    None
}

/// What a `register` holds. Spec §4 fixes the shape; with no output it is
/// D14's placeholder — real shape, empty values — so `when: r.changed` reads
/// false and the reader is reported unprobed rather than skipped.
fn result_value(out: Option<&Output>, status: &Status) -> Value {
    let mut m = BTreeMap::new();
    m.insert("rc".to_string(), Value::from(out.map_or(0, |o| o.rc)));
    m.insert(
        "stdout".to_string(),
        Value::from(out.map_or("", |o| o.stdout.as_str())),
    );
    m.insert(
        "stderr".to_string(),
        Value::from(out.map_or("", |o| o.stderr.as_str())),
    );
    m.insert(
        "changed".to_string(),
        Value::from(matches!(status, Status::Changed)),
    );
    m.insert(
        "skipped".to_string(),
        Value::from(matches!(status, Status::Skipped(_))),
    );
    // D22: `rc: 124` alone cannot tell a step's own `timeout(1)` from
    // provision killing it for exceeding its own `timeout` or `--deadline`.
    // This is the field that can.
    m.insert(
        "timed_out".to_string(),
        Value::from(out.is_some_and(|o| o.how == How::TimedOut)),
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
    #[expect(
        clippy::map_err_ignore,
        reason = "the replacement diagnostic restates the cause"
    )]
    let parsed = Doc::from_str("--prop", text)
        .and_then(|d| d.node().to_value())
        .map_err(|_| bad());
    let value = parsed?;
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
    // Props the caller gave a value for that did not survive. They are not
    // missing -- they are wrong, and that has already been reported at the
    // line the reader has to edit.
    errored: &HashSet<String>,
    errors: &mut Vec<Diag>,
    missing_at: &dyn Fn(&load::PropSchema) -> Diag,
) {
    for schema in &component.props {
        if out.contains_key(&schema.name) || errored.contains(&schema.name) {
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

pub(crate) fn collect_strings(n: N<'static>, out: &mut Vec<N<'static>>) {
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
        entries.sort_by_key(std::fs::DirEntry::file_name);
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
        .and_then(|cwd| p.strip_prefix(cwd).ok().map(crate::error::show))
        .unwrap_or_else(|| crate::error::show(p))
}

#[cfg(test)]
mod tests {
    //! `shell_var` is the whole of §3.2's second half: whether a rendered
    //! path holds a `$NAME` provision will take literally. It is a byte
    //! scanner with four separate ways to decide a `$` is not a variable,
    //! and every one of them was reached only through a whole `validate`
    //! run — one case per end-to-end test, which is the most expensive
    //! possible way to cover a function that takes a `&str`.

    use super::shell_var;

    #[test]
    fn a_bare_name_and_a_braced_one_are_both_found() {
        assert_eq!(shell_var("$CACHE_DIR/hit").as_deref(), Some("CACHE_DIR"));
        assert_eq!(shell_var("${CACHE_DIR}/hit").as_deref(), Some("CACHE_DIR"));
        // The first one, not the last: the message names a variable to
        // rewrite, and the first is the one a reader finds.
        assert_eq!(shell_var("$ONE/$TWO").as_deref(), Some("ONE"));
        // Anywhere in the path, not only at the front.
        assert_eq!(shell_var("/opt/$TOOL/bin").as_deref(), Some("TOOL"));
    }

    #[test]
    fn a_dollar_that_is_not_a_variable_is_just_a_character() {
        // A filename may hold any of these, and refusing one would refuse a
        // path that is correct as written.
        for s in [
            "no dollar here",
            "cost$",          // a lone `$` at the end
            "a $ b",          // a `$` followed by a space
            "$-weird",        // a `$` followed by punctuation
            "${unclosed/hit", // a brace that never closes
            "${}",            // braces around nothing
            "$1/first",       // a shell positional, not an environment name
            "100$/each",      // a digit cannot start a name
        ] {
            assert_eq!(shell_var(s), None, "`{s}` was read as a variable");
        }
    }

    #[test]
    fn a_name_ends_where_the_identifier_does() {
        assert_eq!(shell_var("$HOME/x").as_deref(), Some("HOME"));
        assert_eq!(shell_var("$HOME-old").as_deref(), Some("HOME"));
        assert_eq!(shell_var("$A1_b2.txt").as_deref(), Some("A1_b2"));
        // A multi-byte character ends the name rather than splitting it,
        // which is the reason the scan is byte-wise at all.
        assert_eq!(shell_var("$DIR€/x").as_deref(), Some("DIR"));
        assert_eq!(shell_var("${DIR}€").as_deref(), Some("DIR"));
    }

    #[test]
    fn a_later_dollar_is_still_found_after_an_earlier_one_fails() {
        // The loop continues past a `$` that is not a variable rather than
        // stopping at it -- otherwise a path holding both reports clean.
        assert_eq!(shell_var("$ /then $REAL").as_deref(), Some("REAL"));
        assert_eq!(shell_var("${unclosed $REAL").as_deref(), Some("REAL"));
    }
}
