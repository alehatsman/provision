//! provision — converge one machine from one YAML plan.
//!
//! Three commands over one walk: `validate` checks and runs nothing, `plan`
//! checks and asks the machine questions, `apply` checks and does the work.

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
#[command(name = "provision", version, about = "Converge one machine from one YAML plan")]
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
        #[command(flatten)]
        vars: VarArgs,
    },
    /// Report what would run, probing the machine where an action can.
    Plan {
        plan: PathBuf,
        /// Do not probe the machine; report every step as unprobed.
        #[arg(long)]
        plan_no_probe: bool,
        #[command(flatten)]
        run: RunArgs,
    },
    /// Do the work. Stops at the first failed step.
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
        #[command(flatten)]
        run: RunArgs,
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
    /// Do not print diffs. Accepted by `apply`, which has none to print, so
    /// a script can pass both commands the same arguments.
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
        Selection { tags: self.tags.clone(), skip_tags: self.skip_tags.clone() }
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
            Box::new(output::Both(Box::new(output::json::Json::new(base)), Box::new(text)))
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
            for (k, v) in root.as_map().map_err(|_| {
                Diag::file_level(path, "--vars-file must be a mapping of variables")
            })? {
                m.insert(k.as_scalar_string()?, v.to_value()?);
            }
        }
        for pair in &self.var {
            let (k, v) = pair.split_once('=').ok_or_else(|| {
                Diag::file_level("--var", format!("`{pair}` is not KEY=VALUE"))
            })?;
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
    let cli = Cli::parse();
    match cli.command {
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

        Command::Validate { plan, strict, vars } => {
            let plan = check_exists(&plan)?;
            let selection = Selection { tags: Vec::new(), skip_tags: Vec::new() };
            let mut ex = expander(&vars, Mode::Validate { strict }, selection)?;
            ex.run(&plan)?;
            let base = cwd();
            if ex.diags.is_empty() {
                println!("  ok  {}", rel(&plan, &base));
                return Ok(EXIT_OK);
            }
            report(&ex, &base, Some(&plan));
            Ok(EXIT_USAGE)
        }

        Command::Plan { plan, plan_no_probe, run } => {
            run.apply_color();
            let plan = check_exists(&plan)?;
            let base = cwd();
            let mode = Mode::Plan { probe: !plan_no_probe };
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
            ex.run(&plan)?;
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
            Ok(if ex.summary.has_changes() { EXIT_CHANGES } else { EXIT_OK })
        }

        Command::Apply { plan, ask_sudo_pass, verbose, stream, run } => {
            run.apply_color();
            let plan = check_exists(&plan)?;
            let base = cwd();

            // Spec §7: the walk that finds the sudo steps also validates the
            // whole plan, so a typo in the last step fails before the first
            // step runs. It renders everything and touches nothing.
            let mut check =
                expander(&run.vars, Mode::Validate { strict: false }, run.selection())?;
            check.run(&plan)?;
            if !check.diags.is_empty() {
                report(&check, &base, Some(&plan));
                return Ok(EXIT_USAGE);
            }
            let sudo = Sudo::preflight(check.needs_sudo, ask_sudo_pass)?;

            exec::process::catch_interrupts();
            let mut ex = expander(&run.vars, Mode::Apply, run.selection())?
                .with_runner(Runner { sudo, stream, root_available: true })
                .with_sink(run.sink(base.clone(), verbose, stream));

            let started = Instant::now();
            ex.run(&plan)?;
            ex.summarize(&plan, started.elapsed());

            if !ex.diags.is_empty() {
                eprintln!();
                report(&ex, &base, None);
                return Ok(EXIT_USAGE);
            }
            if ex.summary.interrupted {
                return Ok(EXIT_INTERRUPTED);
            }
            Ok(if ex.summary.failed > 0 { EXIT_FAILED } else { EXIT_OK })
        }
    }
}

fn report(ex: &Expander, base: &Path, plan: Option<&Path>) {
    let mut err = std::io::stderr();
    output::diagnostics(&mut err, &ex.diags, base).ok();
    let n = ex.diags.len();
    match plan {
        Some(p) => eprintln!("\n  {n} problem{} in {}", plural(n), rel(p, base)),
        None => eprintln!("\n  {n} problem{}", plural(n)),
    }
}

fn expander(vars: &VarArgs, mode: Mode, selection: Selection) -> Result<Expander, Diag> {
    let facts = facts::Facts::detect();
    let globals = Rc::new(Globals::new(&facts, vars.collect()?));
    Ok(Expander::new(globals, mode, selection))
}

fn check_exists(plan: &Path) -> Result<PathBuf, Diag> {
    if !plan.exists() {
        return Err(Diag::file_level(plan, "no such plan file"));
    }
    Ok(plan.to_path_buf())
}

fn rel(p: &Path, base: &Path) -> String {
    p.strip_prefix(base).unwrap_or(p).display().to_string()
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}
