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
/// The shell's convention for a process ended by SIGINT.
const EXIT_INTERRUPTED: u8 = 130;

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
        plan: PathBuf,
        /// Also reject `shell`/`cmd` steps with no idempotency gate.
        #[arg(long)]
        strict: bool,
        /// Set a prop, when the root file is a component rather than a plan.
        /// Checked exactly as `apply` checks it: a required prop with no
        /// value is an error here too, and nothing stands in for it.
        #[arg(long = "prop", value_name = "KEY=VALUE")]
        prop: Vec<String>,
        #[command(flatten)]
        vars: VarArgs,
    },
    /// Report what would run, probing the machine where an action can.
    Plan {
        plan: PathBuf,
        /// Do not probe the machine; report every step as unprobed.
        #[arg(long)]
        plan_no_probe: bool,
        /// Set a prop, when the root file is a component rather than a plan.
        #[arg(long = "prop", value_name = "KEY=VALUE")]
        prop: Vec<String>,
        #[command(flatten)]
        run: RunArgs,
    },
    /// Do the work. Stops at the first failed step, unless `--keep-going`.
    Apply {
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
        /// Set a prop, when the root file is a component rather than a plan.
        #[arg(long = "prop", value_name = "KEY=VALUE")]
        prop: Vec<String>,
        #[command(flatten)]
        run: RunArgs,
    },
    /// Name the components in a directory, with their descriptions.
    List {
        /// A directory of component files. A trailing separator is optional:
        /// the verb already says what the argument is.
        dir: PathBuf,
    },
    /// Print the facts this machine reports.
    Facts {
        #[arg(long)]
        json: bool,
    },
}

#[derive(clap::Args, Default)]
struct RunArgs {
    #[arg(long, value_delimiter = ',')]
    tags: Vec<String>,
    #[arg(long, value_delimiter = ',')]
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
}

impl VarArgs {
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
        Command::List { dir } => list_tasks(&dir, &cwd()),

        Command::Facts { json } => {
            let f = facts::Facts::detect();
            if json {
                let m: std::collections::BTreeMap<_, _> = f.iter().collect();
                println!("{}", serde_json::to_string_pretty(&m).unwrap_or_default());
            } else {
                for (k, v) in f.iter() {
                    // Jinja2 renders booleans Python-style (`True`). Facts are
                    // reported as a plan file would write them.
                    let shown = if v.kind() == minijinja::value::ValueKind::Bool {
                        v.is_true().to_string()
                    } else {
                        v.to_string()
                    };
                    println!("{k:<20} {shown}");
                }
            }
            Ok(EXIT_OK)
        }

        Command::Validate {
            plan,
            strict,
            prop,
            vars,
        } => {
            let plan = check_exists(&plan)?;
            let props = pairs(&prop, "--prop")?;
            let selection = Selection {
                tags: Vec::new(),
                skip_tags: Vec::new(),
            };
            let mut ex = expander(&vars, Mode::Validate { strict }, selection)?;
            walk_root(&mut ex, &plan, &props)?;
            let base = cwd();
            if ex.diags.is_empty() {
                println!("  ok  {}", rel(&plan, &base));
                return Ok(EXIT_OK);
            }
            report(&ex, &base, Some(&plan));
            Ok(EXIT_USAGE)
        }

        Command::Plan {
            plan,
            plan_no_probe,
            prop,
            run,
        } => {
            run.apply_color();
            let plan = check_exists(&plan)?;
            let props = pairs(&prop, "--prop")?;
            let base = cwd();
            let mode = Mode::Plan {
                probe: !plan_no_probe,
            };
            let mut ex = expander(&run.vars, mode, run.selection())?
                // Gates run with their step's sudo (§4), so plan has to know
                // whether root is reachable. Unlike apply it must not fail
                // when it is not: plan is the read-only command, and a root
                // gate it cannot run is reported unprobed rather than fatal.
                .with_runner(Runner {
                    sudo: Sudo::none(),
                    stream: false,
                    root_available: sudo::root_is_reachable(),
                })
                .with_sink(run.sink(base.clone(), false, false));

            let started = Instant::now();
            walk_root(&mut ex, &plan, &props)?;
            ex.summarize(&plan, started.elapsed());

            if !ex.diags.is_empty() {
                eprintln!();
                report(&ex, &base, None);
                return Ok(EXIT_USAGE);
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
            prop,
            run,
        } => {
            run.apply_color();
            let plan = check_exists(&plan)?;
            let props = pairs(&prop, "--prop")?;
            let base = cwd();

            // Spec §7: the walk that finds the sudo steps also validates the
            // whole plan, so a typo in the last step fails before the first
            // step runs. It renders everything and touches nothing.
            let mut check = expander(&run.vars, Mode::Validate { strict: false }, run.selection())?;
            walk_root(&mut check, &plan, &props)?;
            if !check.diags.is_empty() {
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
                .with_sink(run.sink(base.clone(), verbose, stream));

            let started = Instant::now();
            walk_root(&mut ex, &plan, &props)?;
            ex.summarize(&plan, started.elapsed());

            if !ex.diags.is_empty() {
                eprintln!();
                report(&ex, &base, None);
                return Ok(EXIT_USAGE);
            }
            if ex.summary.interrupted {
                return Ok(EXIT_INTERRUPTED);
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
fn walk_root(ex: &mut Expander, root: &Path, props: &[(String, String)]) -> Result<(), Diag> {
    if is_component(root)? {
        return ex.run_component(root, props);
    }
    if let Some((key, _)) = props.first() {
        return Err(Diag::file_level(
            root,
            format!("`--prop {key}=…` but this file is a plan, not a component"),
        )
        .with_note("props are declared at a component's root; a plan takes `--var`"));
    }
    ex.run(root)
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

/// `KEY=VALUE` pairs from a repeatable flag. Shared by `--prop` on `run` and
/// on `validate`, which check the same three things.
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
fn is_component(path: &Path) -> Result<bool, Diag> {
    Ok(yaml::Doc::load(path)?.node().as_map().is_ok())
}

/// `provision list <dir>/`. One line per `.yml` file, sorted by stem: the
/// file stem, then its `description` or nothing. Nothing below the root keys
/// is parsed and nothing is run, so a directory holding one broken file
/// still lists. This is the task runner's "what can I run here", and the
/// only thing `list` does (spec §8).
fn list_tasks(dir: &Path, base: &Path) -> Result<u8, Diag> {
    if !dir.is_dir() {
        return Err(Diag::file_level(dir, "no such directory"));
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| Diag::file_level(dir, format!("cannot read: {e}")))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "yml"))
        .collect();
    // By stem, not by filename: sorting the whole name puts `ci-fast` above
    // `ci`, because `-` sorts below `.`. The name a reader is looking for is
    // the one the listing prints.
    files.sort_by(|a, b| a.file_stem().cmp(&b.file_stem()));

    if files.is_empty() {
        println!("  no tasks in {}", rel(dir, base));
        return Ok(EXIT_OK);
    }
    for path in &files {
        let stem = path.file_stem().unwrap_or_default().to_string_lossy();
        // A file that will not load, or is not a component, is named rather
        // than skipped: a listing that silently omits a file is worse than
        // one that says which file is wrong.
        let note = match yaml::Doc::load(path).and_then(config::load::parse_component) {
            Ok(c) => c.description.unwrap_or_default(),
            Err(_) => "(not a component)".to_string(),
        };
        println!("  {stem:<24}  {note}");
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
    p.strip_prefix(base).unwrap_or(p).display().to_string()
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}
