//! provision — converge one machine from one YAML plan.
//!
//! Three commands over one walk: `validate` checks and runs nothing, `plan`
//! checks and asks the machine questions, `apply` checks and does the work.
//! Each takes a plan or a component as its root, and the root's own shape
//! says which it is (spec §8, D17). `list` and `facts` walk nothing.

mod actions;
mod config;
mod error;
mod exec;
mod expand;
mod facts;
mod output;
mod scope;
mod template;
mod yaml;

use clap::{Parser, Subcommand};
use error::Diag;
use exec::runner::Runner;
use exec::sudo::{self, Sudo};
use expand::{Expander, Mode, Selection};
use output::Sink;
use output::text::{Target, Text};
use scope::{Globals, Map};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::rc::Rc;
use std::time::Instant;

/// Spec §8.
const EXIT_OK: u8 = 0;
const EXIT_FAILED: u8 = 1;
/// "plan found changes", which is why it is not an error (D15).
const EXIT_CHANGES: u8 = 2;
const EXIT_USAGE: u8 = 3;
/// `timeout(1)`'s code, for `--deadline` (D19). The code for a stop from
/// *outside* is not a constant: it is `128 + signal`, from
/// `exec::process::stop_code()`, because 130 and 143 are the difference
/// between Ctrl-C and a runner cancelling the job.
const EXIT_DEADLINE: u8 = 124;

#[derive(Parser)]
#[command(
    name = "provision",
    version,
    about = "Converge one machine from one YAML plan"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Parse, check the schema, render every template, resolve every file.
    Validate {
        /// The plan or component file.
        plan: PathBuf,
        /// Also reject `shell`/`cmd` steps with no idempotency gate.
        ///
        /// On by default when the file's root is a list, which is what makes
        /// it a convergence plan; a mapping root (`description:`/`steps:`) is
        /// a component or task and stays opt-in. This flag only ever turns
        /// the check on.
        #[arg(long)]
        strict: bool,
        /// One JSON object per line on stdout; human output stays on stderr.
        /// `validate` produces no step list, so this is the diagnostics and
        /// nothing else.
        // D25.
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        vars: VarArgs,
    },
    /// Report what would run, probing the machine where an action can.
    Plan {
        /// The plan or component file.
        plan: PathBuf,
        /// Do not probe the machine; report every step as unprobed.
        #[arg(long)]
        plan_no_probe: bool,
        #[command(flatten)]
        run: RunArgs,
    },
    /// Do the work. Stops at the first failed step, unless `--keep-going`.
    Apply {
        /// The plan or component file.
        plan: PathBuf,
        /// Read the sudo password once, instead of requiring `sudo -n`.
        #[arg(long)]
        ask_sudo_pass: bool,
        /// Print each step's captured output, not only a failure's.
        #[arg(long)]
        verbose: bool,
        /// Let each step write straight to the terminal as it runs.
        #[arg(long)]
        stream: bool,
        /// Carry on past a failed step, so one run reports everything that
        /// is broken. Ctrl-C still stops.
        #[arg(long)]
        keep_going: bool,
        #[command(flatten)]
        run: RunArgs,
    },
    /// Name the components in a directory, describe one component's props,
    /// or describe a builtin action's fields.
    List {
        /// A directory of component files, one component file, or the name of
        /// a builtin action (`git`, `pkg`, …). A trailing separator is
        /// optional: the verb already says what the argument is. Omit it
        /// entirely with `--json` to dump the whole vocabulary.
        path: Option<PathBuf>,
        /// One JSON document on stdout. With no path, that document is every
        /// builtin action and every step modifier — what a writer of plan
        /// files needs in one call, rather than a column-aligned table to
        /// scrape or the whole spec to carry.
        // D25.
        #[arg(long)]
        json: bool,
    },
    /// Print the facts this machine reports.
    Facts {
        #[arg(long)]
        json: bool,
    },
}

#[derive(clap::Args, Default)]
struct RunArgs {
    /// Run only steps carrying one of these tags, comma-separated. Untagged
    /// steps are skipped; steps tagged `always` run anyway. A tag no step in
    /// the plan carries is an error, not an empty run.
    #[arg(long, value_delimiter = ',', value_name = "TAG,...")]
    tags: Vec<String>,
    /// Run everything except steps carrying one of these tags,
    /// comma-separated. Wins over `--tags`, and over `always`.
    #[arg(long, value_delimiter = ',', value_name = "TAG,...")]
    skip_tags: Vec<String>,
    /// Drop skipped steps from the output. The summary still counts them.
    #[arg(long)]
    hide_skipped: bool,
    /// Do not print diffs. `apply` prints them too — a `file` or `template`
    /// step shows what it changed — so this is not a plan-only flag.
    #[arg(long)]
    no_diff: bool,
    /// One JSON object per line on stdout; human output moves to stderr.
    #[arg(long)]
    json: bool,
    #[arg(long, value_enum, default_value_t = Color::Auto)]
    color: Color,
    /// A wall clock for the whole run: `30s`, `5m`, `1h`. Steps still carry
    /// their own `timeout`; this bounds the sum of them. The run exits 124,
    /// the same code a killed command reports.
    // Spec §8, D19.
    #[arg(long, value_name = "DURATION")]
    deadline: Option<String>,
    #[command(flatten)]
    vars: VarArgs,
}

#[derive(clap::ValueEnum, Clone, Copy, Default, PartialEq, Eq)]
enum Color {
    #[default]
    Auto,
    Always,
    Never,
}

impl RunArgs {
    fn selection(&self) -> Selection {
        Selection {
            tags: self.tags.clone(),
            skip_tags: self.skip_tags.clone(),
        }
    }

    /// Spec §8: `--deadline` takes §4's duration grammar, through §4's parser.
    /// A bad one is a usage error like any other bad flag value, reported
    /// before anything is walked.
    fn deadline(&self) -> Result<Option<std::time::Duration>, Diag> {
        self.deadline
            .as_deref()
            .map(|s| {
                config::model::duration_from_str(s).map_err(|e| {
                    let msg = match e.cause() {
                        Some(cause) => format!("`{s}` is not a duration: {cause}"),
                        None => format!("`{s}` is not a duration"),
                    };
                    Diag::file_level("--deadline", msg).with_note(e.note())
                })
            })
            .transpose()
    }

    /// Spec §9.1: `NO_COLOR` and `--color=never` are honored. anstream reads
    /// the first; this is the second.
    fn apply_color(&self) {
        let choice = match self.color {
            Color::Auto => anstream::ColorChoice::Auto,
            Color::Always => anstream::ColorChoice::Always,
            Color::Never => anstream::ColorChoice::Never,
        };
        choice.write_global();
    }

    fn sink(&self, base: PathBuf, verbose: bool, stream: bool) -> Box<dyn Sink> {
        let target = if self.json { Target::Err } else { Target::Out };
        let mut text = Text::new(target, base.clone())
            .verbose(verbose)
            .hide_skipped(self.hide_skipped)
            .no_diff(self.no_diff);
        if stream {
            // The child owns the terminal now; a spinner would fight it.
            text = text.no_spinner();
        }
        if self.json {
            Box::new(output::Both(
                Box::new(output::json::Json::new(base)),
                Box::new(text),
            ))
        } else {
            Box::new(text)
        }
    }
}

#[derive(clap::Args, Default)]
struct VarArgs {
    /// Set a variable. Repeatable; later wins.
    #[arg(long = "var", value_name = "KEY=VALUE")]
    var: Vec<String>,
    /// Load a YAML mapping of variables. Repeatable; later wins.
    #[arg(long = "vars-file", value_name = "PATH")]
    vars_file: Vec<PathBuf>,
    /// Set a prop, when the root file is a component rather than a plan.
    /// Repeatable. A required prop with no value is an error, and nothing
    /// stands in for it.
    #[arg(long = "prop", value_name = "KEY=VALUE")]
    prop: Vec<String>,
}

impl VarArgs {
    fn props(&self) -> Result<Vec<(String, String)>, Diag> {
        pairs(&self.prop, "--prop")
    }

    /// Spec §3.3: the command line is the highest precedence layer.
    fn collect(&self) -> Result<Map, Diag> {
        let mut m = Map::new();
        for path in &self.vars_file {
            let doc = yaml::Doc::load(path)?;
            let root = doc.node();
            if root.is_null() {
                continue;
            }
            #[expect(
                clippy::map_err_ignore,
                reason = "the replacement diagnostic restates the cause"
            )]
            let pairs = root.as_map().map_err(|_| {
                Diag::file_level(path, "--vars-file must be a mapping of variables")
            })?;
            for (k, v) in pairs {
                m.insert(k.as_scalar_string()?, v.to_value()?);
            }
        }
        for pair in &self.var {
            let (k, v) = pair
                .split_once('=')
                .ok_or_else(|| Diag::file_level("--var", format!("`{pair}` is not KEY=VALUE")))?;
            m.insert(k.to_string(), minijinja::Value::from(v));
        }
        Ok(m)
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(code),
        Err(d) => {
            eprintln!("  error: {}", d.display_from(&cwd()));
            ExitCode::from(EXIT_USAGE)
        }
    }
}

fn cwd() -> PathBuf {
    std::env::current_dir().unwrap_or_default()
}

fn run() -> Result<u8, Diag> {
    // Spec §8 has said `3` is the usage code since phase 0, and clap's own
    // default is `2` — which to a script reading the code means "plan found
    // changes". `provision plan --bogus x.yml` was answering the wrong
    // question. `--help` and `--version` arrive here as errors too; those
    // are exit 0 and go to stdout, which `print` already knows.
    let cli = match Cli::try_parse() {
        Ok(c) => c,
        Err(e) => {
            #[expect(
                clippy::let_underscore_must_use,
                reason = "reporting a usage error; if that write fails there is nowhere left to report"
            )]
            let _ = e.print();
            return Ok(if e.use_stderr() { EXIT_USAGE } else { EXIT_OK });
        }
    };
    match cli.command {
        Command::List { path, json } => match (path, json) {
            (None, true) => Ok(vocabulary()),
            (None, false) => Err(Diag::file_level("list", "needs a path, or `--json`")
                .with_note("a directory of components, a component file, an action name, or `--json` for all of them")),
            (Some(p), json) => list(&p, &cwd(), json),
        },

        Command::Facts { json } => {
            let f = facts::Facts::detect();
            if json {
                let m: std::collections::BTreeMap<_, _> = f.iter().collect();
                output::line(format_args!(
                    "{}",
                    serde_json::to_string_pretty(&m).unwrap_or_default()
                ));
            } else {
                for (k, v) in f.iter() {
                    // Jinja2 renders booleans Python-style (`True`). Facts are
                    // reported as a plan file would write them.
                    let shown = if v.kind() == minijinja::value::ValueKind::Bool {
                        v.is_true().to_string()
                    } else {
                        v.to_string()
                    };
                    output::line(format_args!("{k:<20} {shown}"));
                }
                // Not facts, and the reason to print them here anyway: these
                // are the other names a plan can read, and a reader who runs
                // `provision facts` to find out what is available has just
                // been given an answer that omits two of them. The `--json`
                // form is a contract (D25) and keeps only the facts.
                output::line(format_args!(
                    "\nalso readable, and not facts:\n\
                     {:<20} {}\n\
                     {:<20} {}\n\
                     {:<20} {}",
                    "env.NAME",
                    "this process's environment, e.g. {{ env.HOME }}",
                    "facts.NAME",
                    "the facts above, under a namespace no `vars` can shadow",
                    "component_dir",
                    "the directory of the component being run, inside a `use` only",
                ));
            }
            Ok(EXIT_OK)
        }

        Command::Validate {
            plan,
            strict,
            json,
            vars,
        } => {
            let plan = check_exists(&plan)?;
            let props = vars.props()?;
            let selection = Selection {
                tags: Vec::new(),
                skip_tags: Vec::new(),
            };
            // D3, and its reversal: a convergence plan (a list root) has
            // state to declare and `--strict` catching a forgotten gate is
            // the whole point, so it is on by default there. A task or
            // component file legitimately has none, and stays opt-in. The
            // flag only ever forces it on, never off.
            let mut ex = expander(&vars, Mode::Validate, selection)?;
            let root = Root::of(ex.root_doc(&plan)?);
            let by_root = root.strict_by_default();
            let mut ex = ex.strict(strict || by_root).gate_from_root(by_root);
            walk_root(&mut ex, root, &plan, &props)?;
            let base = cwd();
            if ex.diags.is_empty() {
                // Nothing to say on the stream: no diagnostics is the whole
                // of what `validate` reports, and silence plus exit 0 is the
                // answer. Without `--json` the human `ok` line goes to stdout.
                if !json {
                    output::line(format_args!("  ok  {}", rel(&plan, &base)));
                }
                return Ok(EXIT_OK);
            }
            if json {
                report_json(&ex, &base);
            }
            report(&ex, &base, Some(&plan));
            Ok(EXIT_USAGE)
        }

        Command::Plan {
            plan,
            plan_no_probe,
            run,
        } => {
            run.apply_color();
            let plan = check_exists(&plan)?;
            let props = run.vars.props()?;
            let base = cwd();
            let mode = Mode::Plan {
                probe: !plan_no_probe,
            };
            let mut ex = expander(&run.vars, mode, run.selection())?;
            let root = Root::of(ex.root_doc(&plan)?);
            let mut ex = ex
                // D26. Plan does not need a preflight the way apply does: it
                // executes nothing, so a diag is non-fatal mid-walk and the
                // report after it turns the walk into exit 3, which is the
                // path every other plan-time diagnostic already takes.
                .strict(root.strict_by_default())
                .gate_from_root(root.strict_by_default())
                // Gates run with their step's sudo (§4), so plan has to know
                // whether root is reachable. Unlike apply it must not fail
                // when it is not: plan is the read-only command, and a root
                // gate it cannot run is reported unprobed rather than fatal.
                .with_runner(Runner {
                    sudo: Sudo::none(),
                    stream: false,
                    root_available: sudo::root_is_reachable(),
                })
                .with_deadline(run.deadline()?)
                .with_sink(run.sink(base.clone(), false, false));

            // Plan runs gates (§4), and every child is put in its own process
            // group, so the terminal's Ctrl-C never reaches one. Without a
            // handler provision died where it stood and a hung `unless`
            // outlived it, unowned — the orphan this handler exists to
            // prevent under `apply`, and a gate is just as much a child.
            exec::process::catch_interrupts();
            let started = Instant::now();
            walk_root(&mut ex, root, &plan, &props)?;

            // Before `summarize`, not after. A summary of a walk that found
            // problems is a summary of nothing, and under `--json` a
            // consumer reading the summary first would have drawn its
            // conclusion before the problems arrived. `validate` already
            // reports this way; this is `plan` agreeing with it.
            if !ex.diags.is_empty() {
                if run.json {
                    report_json(&ex, &base);
                }
                report(&ex, &base, None);
                return Ok(EXIT_USAGE);
            }
            ex.summarize(&plan, started.elapsed());
            // Spec §8, the exit-code precedence: a stop from outside beats
            // the clock, which beats a failed step — "the reason the run
            // ended", the same rule `apply` reads.
            if ex.summary.interrupted {
                return Ok(exec::process::stop_code());
            }
            if ex.summary.deadline_exceeded {
                return Ok(EXIT_DEADLINE);
            }
            // Only an `assert` can fail at plan time. The walk kept going
            // (§6.7), so this is the whole plan's verdict, not the first
            // step's — and it is a failure, not "changes were found".
            if ex.summary.failed > 0 {
                return Ok(EXIT_FAILED);
            }
            // D15: `--plan-no-probe` inspected nothing, so it claims nothing.
            if plan_no_probe {
                return Ok(EXIT_OK);
            }
            Ok(if ex.summary.has_changes() {
                EXIT_CHANGES
            } else {
                EXIT_OK
            })
        }

        Command::Apply {
            plan,
            ask_sudo_pass,
            verbose,
            stream,
            keep_going,
            run,
        } => {
            run.apply_color();
            let plan = check_exists(&plan)?;
            let props = run.vars.props()?;
            // Read here, used below: a typo'd `--deadline` is a usage error,
            // and finding it after the sudo prompt would mean asking for a
            // password to run nothing.
            let deadline = run.deadline()?;
            let base = cwd();

            // Spec §7: the walk that finds the sudo steps also validates the
            // whole plan, so a typo in the last step fails before the first
            // step runs. It renders everything and touches nothing.
            // D26: the gate check belongs to this walk and not to the one
            // that executes. A diag raised during the apply walk only stops
            // it at the *next* step, so the ungated command would already
            // have run — which is the regression D20 named. Here it lands
            // before `Sudo::preflight`, before `catch_interrupts`, and
            // before the apply expander exists.
            let mut check = expander(&run.vars, Mode::Validate, run.selection())?;
            let root = Root::of(check.root_doc(&plan)?);
            let mut check = check
                .strict(root.strict_by_default())
                .gate_from_root(root.strict_by_default());
            walk_root(&mut check, root, &plan, &props)?;
            if !check.diags.is_empty() {
                // The preflight runs before the apply expander and its sink
                // exist, which is why `apply --json` on a bad file used to
                // emit nothing at all on stdout — exit 3 and prose on
                // stderr, and the stream a runner was reading stayed empty.
                if run.json {
                    report_json(&check, &base);
                }
                report(&check, &base, Some(&plan));
                return Ok(EXIT_USAGE);
            }
            let sudo = Sudo::preflight(check.needs_sudo, ask_sudo_pass)?;

            exec::process::catch_interrupts();
            let mut ex = expander(&run.vars, Mode::Apply, run.selection())?
                .keep_going(keep_going)
                .with_runner(Runner {
                    sudo,
                    stream,
                    root_available: true,
                })
                // The clock starts here, after the preflight, on purpose: a
                // password typed at a prompt is the operator's time, not the
                // job's.
                .with_deadline(deadline)
                .with_sink(run.sink(base.clone(), verbose, stream));

            // The shape of the run, from the preflight that already visited
            // every step. Printed before the first one, so an operator sees
            // how much is about to happen while it still has not, and so a
            // postmortem finds it at the top of the log. It claims nothing
            // about what will *change*: `plan` is best-effort by D11 and
            // explicitly unprobed for a register-gated step by D14, so a diff
            // promised before the run is one provision cannot keep.
            ex.manifest(&plan, &check.manifest);

            let started = Instant::now();
            walk_root(&mut ex, root, &plan, &props)?;

            if !ex.diags.is_empty() {
                if run.json {
                    report_json(&ex, &base);
                }
                report(&ex, &base, None);
                return Ok(EXIT_USAGE);
            }
            ex.summarize(&plan, started.elapsed());
            // Spec §8, the exit-code precedence: a stop from outside beats the
            // clock, which beats a failed step. Read down, the rule is "the
            // reason the run ended", and a signal is always a better answer
            // than whatever the walk was doing when it arrived.
            if ex.summary.interrupted {
                return Ok(exec::process::stop_code());
            }
            if ex.summary.deadline_exceeded {
                return Ok(EXIT_DEADLINE);
            }
            Ok(if ex.summary.failed > 0 {
                EXIT_FAILED
            } else {
                EXIT_OK
            })
        }
    }
}

/// Spec §8, D17: the root file is a plan or a component, and its own shape
/// says which — a sequence is a plan, a mapping is a component. That is the
/// same distinction both parsers already make in their own error messages,
/// so no command needs a flag saying what it was handed, and every command
/// walks the root the same way.
///
/// `--prop` on a plan is a usage error rather than a value quietly dropped:
/// a plan has no props to set, and the caller who typed one believes it took
/// effect.
fn walk_root(
    ex: &mut Expander,
    root: Root,
    path: &Path,
    props: &[(String, String)],
) -> Result<(), Diag> {
    let out = walk_root_inner(ex, root, path, props);
    // One place, after the whole walk: naming a register the walk saw needs
    // the set of every register bound anywhere, which only exists once the
    // walk is over. Mid-walk the fatality checks read `diags` for whether it
    // is empty, and a note changes neither that nor the count.
    ex.explain_scoped_registers();
    out
}

fn walk_root_inner(
    ex: &mut Expander,
    root: Root,
    path: &Path,
    props: &[(String, String)],
) -> Result<(), Diag> {
    if root == Root::Component {
        return ex.run_component(path, props);
    }
    if let Some((key, _)) = props.first() {
        return Err(Diag::file_level(
            path,
            format!("`--prop {key}=…` but this file is a plan, not a component"),
        )
        .with_note("props are declared at a component's root; a plan takes `--var`"));
    }
    ex.run(path)
}

/// D25: under `--json` every problem rides the stream as its own event,
/// before the summary, because a consumer that reads the summary first and
/// finds a clean one has already drawn the wrong conclusion.
fn report_json(ex: &Expander, base: &Path) {
    let mut sink: Box<dyn Sink> = Box::new(output::json::Json::new(base.to_path_buf()));
    for d in ex.diags.iter() {
        sink.diagnostic(d, base);
    }
}

fn report(ex: &Expander, base: &Path, plan: Option<&Path>) {
    let mut err = std::io::stderr();
    #[expect(
        clippy::unused_result_ok,
        reason = "writing diagnostics to the stream diagnostics go to"
    )]
    output::diagnostics(&mut err, &ex.diags, base).ok();
    let n = ex.diags.len();
    match plan {
        // "in <plan>" read as the problems' location, which is wrong the
        // moment one of them is in an imported file — and each error line
        // already carries its own `file:line:col`. "validating" names the
        // entry point without claiming anything about where the faults are.
        Some(p) => eprintln!("\n  {n} problem{} validating {}", plural(n), rel(p, base)),
        None => eprintln!("\n  {n} problem{}", plural(n)),
    }
}

fn expander(vars: &VarArgs, mode: Mode, selection: Selection) -> Result<Expander, Diag> {
    let facts = facts::Facts::detect();
    let globals = Rc::new(Globals::new(&facts, vars.collect()?));
    Ok(Expander::new(globals, mode, selection))
}

/// `KEY=VALUE` pairs from a repeatable flag, for `--prop` on every verb that
/// takes one.
fn pairs(given: &[String], flag: &str) -> Result<Vec<(String, String)>, Diag> {
    given
        .iter()
        .map(|pair| {
            pair.split_once('=')
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .ok_or_else(|| Diag::file_level(flag, format!("`{pair}` is not KEY=VALUE")))
        })
        .collect()
}

/// D17: a component is a mapping and a plan is a sequence. Reading the root
/// node is enough to tell, and it is the same distinction both parsers make
/// in their own error messages.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Root {
    Plan,
    Component,
}

impl Root {
    /// Spec §8: the shape is read once per command. Two things ask for it —
    /// `strict_by_default` and which entry point `walk_root` takes — and the
    /// answer cannot change between them, so it is derived once and carried.
    ///
    /// The `Doc` comes from the expander's own `Loader` (`root_doc`), not a
    /// fresh `Doc::load`: the walk is about to load the same path, and a
    /// direct parse here both repeated the work and leaked a second copy of
    /// the file for the process lifetime (#62).
    fn of(doc: &'static yaml::Doc) -> Root {
        if doc.node().as_map().is_ok() {
            Root::Component
        } else {
            Root::Plan
        }
    }

    /// D21, D26: a plan has state to declare, and `--strict` catching a
    /// forgotten gate is the whole point there, so it is on by default for a
    /// list root under every verb that walks it. A task or component file
    /// legitimately has none and stays opt-in. `--strict` on the command line
    /// only ever turns it on, never off.
    fn strict_by_default(self) -> bool {
        self == Root::Plan
    }
}

/// `provision list` at its two granularities (spec §8): a directory says what
/// can be run here, a file says what one of them takes. One verb, because it
/// is one question asked with two amounts of precision, and the directory
/// form used to answer only the first half of it.
fn list(path: &Path, base: &Path, json: bool) -> Result<u8, Diag> {
    if path.is_dir() {
        return list_tasks(path, base, json);
    }
    if !path.exists() {
        // Spec §8: a path that exists wins, so only a bare name matching no
        // file is looked up as a builtin. `provision list git` is the first
        // thing an author of a plan reaches for, and a builtin is not a file
        // to open.
        if let Some(name) = path.to_str()
            && let Some(doc) = config::model::action_doc(name)
        {
            if json {
                output::line(format_args!("{}", action_json(name, doc)));
                return Ok(EXIT_OK);
            }
            return Ok(describe_action(name, doc));
        }
        return Err(
            Diag::file_level(path, "no such file or directory").with_note(
                "`list` takes a directory of components, one component file, \
             or the name of a builtin action",
            ),
        );
    }
    describe_component(path, json)
}

/// `provision list <action>`. The summary, then one line per body field:
/// name, what it is (required, optional, or the one-of group it belongs to),
/// and a short gloss — the same shape `list <component.yml>` prints for a
/// prop, because it is the same question.
///
/// The fields come from the table the unknown-key check reads (spec §8), so
/// a key that is listed here is a key `validate` accepts.
/// One action as a document: the same table `describe_action` prints, in the
/// shape something other than a person can read.
fn action_json(name: &str, doc: &config::model::ActionDoc) -> serde_json::Value {
    serde_json::json!({
        "name": name,
        "summary": doc.summary,
        "example": doc.example,
        "body": doc.shape,
        "fields": doc.fields.iter().map(|f| serde_json::json!({
            "name": f.name,
            "need": f.need,
            "note": f.note,
        })).collect::<Vec<_>>(),
    })
}

/// `provision list --json` with no path: every builtin action and every step
/// modifier, in one document.
///
/// D25's "verbs that answer return a document". A writer of plan files needs
/// the envelope at least as much as it needs the eleven actions — `when`,
/// `register`, `creates`, `unless` and the rest are what make a step correct
/// rather than merely parseable — and until this existed nothing described
/// them anywhere a program could read.
fn vocabulary() -> u8 {
    let doc = serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "actions": config::model::action_docs()
            .map(|(name, d)| action_json(name, d))
            .collect::<Vec<_>>(),
        "structural": config::model::STRUCTURAL_KEYS,
        "modifiers": config::model::MODIFIERS.iter().map(|m| serde_json::json!({
            "name": m.name,
            "applies": m.applies,
            "note": m.note,
        })).collect::<Vec<_>>(),
    });
    output::line(format_args!(
        "{}",
        serde_json::to_string_pretty(&doc).unwrap_or_default()
    ));
    EXIT_OK
}

fn describe_action(name: &str, doc: &config::model::ActionDoc) -> u8 {
    output::line(format_args!("  {name:<24}  {}", doc.summary));
    if let Some(shape) = doc.shape {
        output::line(format_args!("\n  body: {shape}"));
    }
    if doc.fields.is_empty() {
        return EXIT_OK;
    }
    output::line(format_args!("\n  fields:"));
    for f in doc.fields {
        output::line(format_args!(
            "    {:<22}  {:<18}  {}",
            f.name, f.need, f.note
        ));
    }
    EXIT_OK
}

/// `provision list <component.yml>`. The description, then one line per prop:
/// name, type, and either `required` or the default **as written** — §3.2 says
/// a default is data in the file and is never rendered, so this prints the
/// four braces when that is what is there rather than implying a value the
/// component would not produce.
///
/// Nothing below the root keys is parsed and nothing runs, exactly as the
/// directory form promises.
fn describe_component(path: &Path, json: bool) -> Result<u8, Diag> {
    let doc = yaml::Doc::load(path)?;
    // A plan gets the same answer `--prop` on a plan already gets: it declares
    // no props, and asking one for its interface is that same mistake.
    if doc.node().as_seq().is_ok() {
        return Err(
            Diag::file_level(path, "this file is a plan, not a component")
                .with_note("props are declared at a component's root; a plan takes `--var`"),
        );
    }
    let component = config::load::parse_component(doc)?;
    let (globals, uncertain) = caller_globals(&component);
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();

    // D25: verbs that answer return a document. `--json` used to be threaded
    // only into the bare-action-name branch, so `list --json <component.yml>`
    // and `list --json <dir>/` silently printed the human listing — a flag
    // accepted and dropped, which is the exact shape §4 calls "a typo that
    // costs an afternoon".
    if json {
        output::line(format_args!(
            "{}",
            component_json(&stem, &component, &globals, uncertain)
        ));
        return Ok(EXIT_OK);
    }

    let description = component.description.clone().unwrap_or_default();
    output::line(format_args!("  {stem:<24}  {description}"));

    if component.props.is_empty() {
        output::line(format_args!("\n  no props"));
        report_globals(&globals, uncertain);
        return Ok(EXIT_OK);
    }
    output::line(format_args!("\n  props:"));
    for p in &component.props {
        // `required` and a default are mutually exclusive — `parse_component`
        // rejects a prop carrying both — so this is a choice, never a join.
        let requirement = match &p.default {
            Some(d) => format!("default: {}", as_written(*d)),
            None if p.required => "required".to_string(),
            // Neither: optional and unset, which reads as undefined inside the
            // component. Saying "optional" out loud beats an empty column.
            None => "optional".to_string(),
        };
        let name = &p.name;
        output::line(format_args!(
            "    {name:<22}  {:<6}  {requirement}",
            p.ty.name()
        ));
        if let Some(d) = &p.description {
            output::line(format_args!("    {:<22}  {d}", ""));
        }
    }
    report_globals(&globals, uncertain);
    Ok(EXIT_OK)
}

/// #41: the names a component reads from whoever calls it, beyond its
/// declared props.
///
/// A component's `props:` block advertises itself as the component's
/// interface and is not one — nothing stops a step inside from reading a
/// caller's `vars`, and §3.1 says a `use` child sees the parent's variables.
/// So `validate caller.yml` passed while `validate --prop tool=x
/// lib/comp.yml` failed on a name `list` never mentioned, and the only way
/// to learn a component's real arguments was to run it and read the error.
///
/// Static and conservative: the names every template in the component reads,
/// minus the ones that do not come from a caller — its own `vars` and
/// `register` bindings, the namespaces (`props`, `env`, `facts`,
/// `component_dir`) and the facts, which the machine supplies. What is left
/// is what the caller has to have set.
fn caller_globals(component: &config::load::Component) -> (Vec<String>, bool) {
    let engine = template::Engine::new();
    let facts = facts::Facts::detect();
    let mut bound: std::collections::HashSet<String> = facts
        .iter()
        .map(|(k, _)| k.clone())
        .chain(
            ["props", "env", "facts", "component_dir", "result", "item"]
                .into_iter()
                .map(String::from),
        )
        .collect();

    // A name the component binds itself is not the caller's to supply. Both
    // bind before the steps that read them, so one pass in file order is
    // enough — and a name read *before* it is bound is a fault this function
    // is not here to diagnose.
    let mut reads: Vec<String> = Vec::new();
    let mut uncertain = false;
    for step in &component.steps {
        if let Some(n) = step.mods.register.and_then(|n| n.as_str().ok()) {
            bound.insert(n.to_string());
        }
        if step.key == "vars"
            && let Ok(pairs) = step.body.as_map()
        {
            for (k, _) in pairs {
                if let Ok(name) = k.as_scalar_string() {
                    bound.insert(name);
                }
            }
        }
        let mut sources: Vec<String> = Vec::new();
        let mut strings = Vec::new();
        expand::collect_strings(step.body, &mut strings);
        for m in config::model::modifier_nodes(step) {
            expand::collect_strings(m, &mut strings);
        }
        for sn in strings {
            if let Ok(src) = sn.as_str()
                && config::model::is_template(sn)
            {
                sources.push(src.to_string());
            }
        }

        // The same boundary D14 had: a step's fields are not the whole of
        // what it reads. `zsh/index.yml` declares two props and its real
        // caller inputs — `editor`, `dotfiles_dir` — appear only inside the
        // `.j2` files it renders. A list that stopped at the fields would
        // have reported the component clean and been wrong about exactly the
        // components that have the problem.
        if step.key == "template"
            && let Some(src_at) = step.body.get("src")
            && let Ok(rel) = src_at.as_str()
        {
            match rel.split_once("{{") {
                None => {
                    let path = config::load::resolve(src_at.file, rel);
                    match std::fs::read_to_string(&path) {
                        Ok(text) => sources.push(text),
                        Err(_) => uncertain = true,
                    }
                }
                // `src: ./{{ props.variant }}.j2` — same shape as a templated
                // `vars_file`, same answer: the directory is fixed even when
                // the file is not, so every candidate in it is read. A name
                // any of them reads is one the caller may have to supply.
                Some((prefix, _)) => {
                    let dir = config::load::resolve(src_at.file, prefix);
                    let mut any = false;
                    if let Ok(entries) = std::fs::read_dir(&dir) {
                        for e in entries.flatten() {
                            let p = e.path();
                            if p.is_file()
                                && let Ok(text) = std::fs::read_to_string(&p)
                            {
                                sources.push(text);
                                any = true;
                            }
                        }
                    }
                    uncertain |= !any;
                }
            }
        }
        // A `vars_file` binds names this component then reads. When its path
        // is a template it cannot be opened here, and a name it would have
        // bound shows up below as if the caller had to supply it.
        if step.key == "vars_file"
            && let Ok(rel) = step.body.as_str()
        {
            {
                let read_keys = |path: &Path, bound: &mut std::collections::HashSet<String>| {
                    let Ok(doc) = yaml::Doc::load(path) else {
                        return false;
                    };
                    let Ok(pairs) = doc.node().as_map() else {
                        return false;
                    };
                    for (k, _) in pairs {
                        if let Ok(name) = k.as_scalar_string() {
                            bound.insert(name);
                        }
                    }
                    true
                };
                match rel.split_once("{{") {
                    None => {
                        let path = config::load::resolve(step.body.file, rel);
                        uncertain |= !read_keys(&path, &mut bound);
                    }
                    // `./vars/{{ distro }}.yml` — the file is chosen at run
                    // time and the directory is not. Every candidate in it
                    // binds the same names in practice (that is the point of
                    // the pattern), so reading all of them is the honest
                    // over-approximation: a name any of them binds is not one
                    // the caller has to supply. Without this, `zsh` reported
                    // `zsh_plugin_dir` as a caller input when its own
                    // os-selected vars file sets it.
                    Some((prefix, _)) => {
                        let dir = config::load::resolve(step.body.file, prefix);
                        let mut any = false;
                        if let Ok(entries) = std::fs::read_dir(&dir) {
                            for e in entries.flatten() {
                                let p = e.path();
                                if p.extension().is_some_and(|x| x == "yml" || x == "yaml") {
                                    any |= read_keys(&p, &mut bound);
                                }
                            }
                        }
                        uncertain |= !any;
                    }
                }
            }
        }

        for src in sources {
            for name in engine.undeclared_in_template(&src).into_iter().flatten() {
                if !bound.contains(&name) && !reads.contains(&name) {
                    reads.push(name);
                }
            }
        }
    }
    reads.sort();
    reads.retain(|n| !bound.contains(n));
    (reads, uncertain)
}

fn report_globals(globals: &[String], uncertain: bool) {
    if globals.is_empty() && !uncertain {
        return;
    }
    output::line(format_args!("\n  reads from the caller:"));
    if globals.is_empty() {
        output::line(format_args!("    none found"));
    }
    for g in globals {
        output::line(format_args!("    {g}"));
    }
    // D11's house answer: say what is not known rather than imply the list is
    // complete. A `vars_file` or a `template` `src` whose path is itself a
    // template cannot be opened without the scope a call site supplies.
    if uncertain {
        output::line(format_args!(
            "    {:<22}  (a path this component builds at run time could not be read, so this list may be wrong in either direction)",
            ""
        ));
    }
}

/// One component as a document (D25). Carries `reads` beside `props`,
/// because a props list that presents itself as the interface and is not one
/// is the thing #41 filed — and a machine-readable listing that inherited
/// that blind spot would be worse than the human one, which at least gets
/// read by someone who can go and look.
fn component_json(
    stem: &str,
    component: &config::load::Component,
    globals: &[String],
    uncertain: bool,
) -> serde_json::Value {
    serde_json::json!({
        "name": stem,
        "description": component.description,
        "props": component.props.iter().map(|p| serde_json::json!({
            "name": p.name,
            "type": p.ty.name(),
            "required": p.required,
            "default": p.default.map(as_written),
            "description": p.description,
        })).collect::<Vec<_>>(),
        // The caller variables the component reads beyond its props, and
        // whether the scan could see everything (D11: say what is not known).
        "reads": globals,
        "reads_complete": !uncertain,
    })
}

/// A default's source text. Spec §3.2: taken as written, never rendered.
fn as_written(n: yaml::N<'static>) -> String {
    n.as_scalar_string()
        .or_else(|_| n.to_value().map(|v| v.to_string()))
        .unwrap_or_default()
}

/// `provision list <dir>/`. One line per component, sorted by the name it
/// prints: that name, then its `description` or nothing. Nothing below the
/// root keys is parsed and nothing is run, so a directory holding one broken
/// file still lists. This is the task runner's "what can I run here" (§8).
///
/// Two layouts, because this repo's own examples use both. A `.yml` file in
/// the directory is one component, named by its stem. A subdirectory holding
/// `index.yml` is also one component, named by the *directory* — that is the
/// layout a component shipping its own `templates/` has to use, since a
/// `src:` resolves relative to the component file. Naming those by stem would
/// print `index` for every one of them, which is why this looks at the
/// directory instead.
///
/// One level down, and `index.yml` by name rather than a recursive sweep:
/// `zsh/` also holds `vars/arch.yml` and its friends, which are a component's
/// data and not components, and a sweep would list them all as
/// `(not a component)`.
fn list_tasks(dir: &Path, base: &Path, json: bool) -> Result<u8, Diag> {
    let mut found: Vec<(String, PathBuf)> = Vec::new();
    for entry in std::fs::read_dir(dir)
        .map_err(|e| Diag::file_level(dir, format!("cannot read: {e}")))?
        .filter_map(Result::ok)
    {
        let path = entry.path();
        if path.is_dir() {
            let index = path.join("index.yml");
            if index.is_file()
                && let Some(name) = path.file_name()
            {
                found.push((name.to_string_lossy().into_owned(), index));
            }
        } else if path.extension().is_some_and(|x| x == "yml")
            && let Some(stem) = path.file_stem()
        {
            found.push((stem.to_string_lossy().into_owned(), path));
        }
    }
    // By the printed name, not by filename: sorting the whole name puts
    // `ci-fast` above `ci`, because `-` sorts below `.`. The name a reader is
    // looking for is the one the listing prints.
    found.sort_by(|a, b| a.0.cmp(&b.0));

    if json {
        // One document, not one object per component: `list` answers rather
        // than walks (D25), and a directory with nothing in it is an empty
        // array rather than silence.
        let items: Vec<serde_json::Value> = found
            .iter()
            .map(|(name, path)| {
                match yaml::Doc::load(path).and_then(config::load::parse_component) {
                    Ok(c) => {
                        let (globals, uncertain) = caller_globals(&c);
                        component_json(name, &c, &globals, uncertain)
                    }
                    // Named, not skipped, for the same reason the text form
                    // names it: a listing that silently omits a file is worse
                    // than one that says which file is wrong.
                    Err(e) => serde_json::json!({
                        "name": name,
                        "error": e.msg,
                    }),
                }
            })
            .collect();
        output::line(format_args!(
            "{}",
            serde_json::json!({ "dir": rel(dir, base).clone(), "components": items })
        ));
        return Ok(EXIT_OK);
    }

    if found.is_empty() {
        output::line(format_args!("  no components in {}", rel(dir, base)));
        return Ok(EXIT_OK);
    }
    for (name, path) in &found {
        // A file that will not load, or is not a component, is named rather
        // than skipped: a listing that silently omits a file is worse than
        // one that says which file is wrong.
        let note = match yaml::Doc::load(path).and_then(config::load::parse_component) {
            Ok(c) => c.description.unwrap_or_default(),
            Err(_) => "(not a component)".to_string(),
        };
        output::line(format_args!("  {name:<24}  {note}"));
    }
    Ok(EXIT_OK)
}

/// The root of a walk is one file — a plan or a component. A directory is
/// the listing's argument and nothing else's, so it is named here rather
/// than left to fail three layers down as "not a mapping".
fn check_exists(plan: &Path) -> Result<PathBuf, Diag> {
    if !plan.exists() {
        return Err(Diag::file_level(plan, "no such plan file"));
    }
    if plan.is_dir() {
        return Err(Diag::file_level(plan, "is a directory")
            .with_note("`provision list <dir>/` names the components in one"));
    }
    Ok(plan.to_path_buf())
}

fn rel(p: &Path, base: &Path) -> String {
    // D29: the separator a reader sees is `/` on every platform.
    error::show(p.strip_prefix(base).unwrap_or(p))
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}
