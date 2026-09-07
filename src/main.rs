//! provision — converge one machine from one YAML plan.
//!
//! Phase 0: parse, validate, render, and plan without probing. Execution
//! arrives in phase 1; `plan` without `--plan-no-probe` says so rather than
//! reporting a verdict it cannot support.

mod config;
mod error;
mod expand;
mod facts;
mod output;
mod scope;
mod template;
mod yaml;

use clap::{Parser, Subcommand};
use error::Diag;
use expand::{Expander, Mode, Selection};
use scope::{Globals, Map};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::rc::Rc;

/// Spec §8. `2` is "plan found changes", which is why it is not an error.
const EXIT_OK: u8 = 0;
/// Spec §8: usage *or validation* error. An apply failure is 1, and arrives
/// with execution in phase 1.
const EXIT_USAGE: u8 = 3;

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
    /// Report what would run. Phase 0 requires --plan-no-probe.
    Plan {
        plan: PathBuf,
        #[arg(long, value_delimiter = ',')]
        tags: Vec<String>,
        #[arg(long, value_delimiter = ',')]
        skip_tags: Vec<String>,
        /// Do not probe the machine; report every step as unprobed.
        #[arg(long)]
        plan_no_probe: bool,
        #[arg(long)]
        hide_skipped: bool,
        #[command(flatten)]
        vars: VarArgs,
    },
    /// Print the facts this machine reports.
    Facts {
        #[arg(long)]
        json: bool,
    },
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
            let mut ex = expander(&vars, Mode::Validate { strict }, Selection {
                tags: Vec::new(),
                skip_tags: Vec::new(),
            })?;
            ex.run(&plan)?;
            let base = cwd();
            if ex.diags.is_empty() {
                println!("  ok  {}", rel(&plan, &base));
                return Ok(EXIT_OK);
            }
            let mut err = std::io::stderr();
            output::plain::diagnostics(&mut err, &ex.diags, &base).ok();
            eprintln!("\n  {} problem{} in {}", ex.diags.len(), plural(ex.diags.len()), rel(&plan, &base));
            Ok(EXIT_USAGE)
        }

        Command::Plan { plan, tags, skip_tags, plan_no_probe, hide_skipped, vars } => {
            if !plan_no_probe {
                return Err(Diag::file_level(
                    "plan",
                    "probing is not implemented yet (phase 1)",
                )
                .with_note("run `provision plan --plan-no-probe <plan.yml>` for the unprobed list"));
            }
            let plan = check_exists(&plan)?;
            let mut ex = expander(&vars, Mode::Plan, Selection { tags, skip_tags })?;
            ex.run(&plan)?;
            let base = cwd();

            let mut out = std::io::stdout();
            output::plain::steps(&mut out, &ex.steps, hide_skipped, &base).ok();
            output::plain::summary(&mut out, &rel_path(&plan, &base), &ex.steps).ok();

            if !ex.diags.is_empty() {
                let mut err = std::io::stderr();
                eprintln!();
                output::plain::diagnostics(&mut err, &ex.diags, &base).ok();
                eprintln!("\n  {} problem{}", ex.diags.len(), plural(ex.diags.len()));
                return Ok(EXIT_USAGE);
            }
            Ok(EXIT_OK)
        }
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
    rel_path(p, base).display().to_string()
}

fn rel_path(p: &Path, base: &Path) -> PathBuf {
    p.strip_prefix(base).unwrap_or(p).to_path_buf()
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}
