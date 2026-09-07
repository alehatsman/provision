//! `pkg` — ensure packages through one manager. Spec §6.5.
//!
//! One table, one row per manager. Everything below the table is
//! manager-agnostic: it asks a row for its query, install, remove, upgrade
//! and refresh argv, and for how to read the query's output. Adding a manager
//! later is one row, not a module — which is also why the row carries a
//! parser function rather than the rest of the file carrying a `match`.

use super::{Ctx, Effect};
use crate::config::model::Step;
use crate::error::Result;
use crate::template::Engine;
use minijinja::Value;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Present,
    Absent,
    Latest,
}

/// Everything that differs between managers, and nothing that does not.
#[derive(Debug)]
pub struct Manager {
    pub name: &'static str,
    /// The binary that must be on PATH.
    pub bin: &'static str,
    query: &'static [&'static str],
    /// brew keeps casks in a separate list; nobody else has the concept.
    query_cask: Option<&'static [&'static str]>,
    /// Read `name version` pairs out of the query's stdout.
    parse: fn(&str) -> BTreeMap<String, String>,
    install: &'static [&'static str],
    remove: &'static [&'static str],
    upgrade: &'static [&'static str],
    /// `update_cache: true`. None where the manager has no such thing.
    refresh: Option<&'static [&'static str]>,
    cask_flag: Option<&'static str>,
    /// winget installs and uninstalls exactly one id per invocation, so a
    /// `names:` list is a loop rather than one call.
    one_per_call: bool,
    /// Refuses to run as root, so `sudo: true` is a validation error.
    rejects_root: bool,
    /// Never chosen when `manager` is absent (spec §6.5: the AUR is a
    /// decision a plan should have to write down).
    never_default: bool,
}

/// Spec §6.5. The default order is the order of this table, minus the rows
/// marked `never_default`.
pub const MANAGERS: &[Manager] = &[
    Manager {
        name: "pacman",
        bin: "pacman",
        query: &["pacman", "-Q"],
        query_cask: None,
        parse: parse_space_pairs,
        install: &["pacman", "-S", "--noconfirm", "--needed"],
        remove: &["pacman", "-R", "--noconfirm"],
        upgrade: &["pacman", "-S", "--noconfirm"],
        refresh: Some(&["pacman", "-Sy"]),
        cask_flag: None,
        one_per_call: false,
        rejects_root: false,
        never_default: false,
    },
    Manager {
        name: "yay",
        // Both read the same database, so the query is pacman's (spec §6.5).
        bin: "yay",
        query: &["pacman", "-Q"],
        query_cask: None,
        parse: parse_space_pairs,
        install: &["yay", "-S", "--noconfirm", "--needed"],
        remove: &["yay", "-R", "--noconfirm"],
        upgrade: &["yay", "-S", "--noconfirm"],
        refresh: Some(&["yay", "-Sy"]),
        cask_flag: None,
        one_per_call: false,
        rejects_root: true,
        never_default: true,
    },
    Manager {
        name: "apt",
        bin: "apt-get",
        // Plain `-W` also lists removed-but-config packages, which would read
        // as present and make `present` a silent no-op on a package that is
        // not installed. The status column is what rules those out.
        query: &["dpkg-query", "-W", "-f", "${Package}\\t${Version}\\t${Status}\\n"],
        query_cask: None,
        parse: parse_dpkg,
        // `env VAR=…` in front rather than a row field: it survives the
        // sudo wrapper with no new machinery. Without it a fresh machine
        // hits a debconf prompt, which waits on a stdin nobody is holding
        // until the watchdog kills the step ten minutes later.
        install: &["env", "DEBIAN_FRONTEND=noninteractive", "apt-get", "install", "-y"],
        remove: &["env", "DEBIAN_FRONTEND=noninteractive", "apt-get", "remove", "-y"],
        upgrade: &[
            "env", "DEBIAN_FRONTEND=noninteractive", "apt-get", "install", "-y", "--only-upgrade",
        ],
        refresh: Some(&["env", "DEBIAN_FRONTEND=noninteractive", "apt-get", "update"]),
        cask_flag: None,
        one_per_call: false,
        rejects_root: false,
        never_default: false,
    },
    Manager {
        name: "brew",
        bin: "brew",
        query: &["brew", "list", "--formula", "--versions"],
        query_cask: Some(&["brew", "list", "--cask", "--versions"]),
        parse: parse_space_pairs,
        install: &["brew", "install"],
        remove: &["brew", "uninstall"],
        upgrade: &["brew", "upgrade"],
        refresh: Some(&["brew", "update"]),
        cask_flag: Some("--cask"),
        one_per_call: false,
        rejects_root: true,
        never_default: false,
    },
    Manager {
        name: "winget",
        bin: "winget",
        query: &["winget", "list"],
        query_cask: None,
        parse: parse_winget,
        install: &[
            "winget", "install", "-e", "--accept-package-agreements",
            "--accept-source-agreements", "--id",
        ],
        remove: &["winget", "uninstall", "-e", "--id"],
        upgrade: &["winget", "upgrade", "-e", "--id"],
        refresh: None,
        cask_flag: None,
        one_per_call: true,
        rejects_root: false,
        never_default: false,
    },
];

/// Rows are unique by name and live in one static table, so the name is the
/// identity. Deriving this would compare the parser function pointers, which
/// the compiler rightly says means nothing.
impl PartialEq for Manager {
    fn eq(&self, other: &Manager) -> bool {
        self.name == other.name
    }
}

impl Eq for Manager {}

fn find(name: &str) -> Option<&'static Manager> {
    MANAGERS.iter().find(|m| m.name == name)
}

// ── query parsers, one per row ────────────────────────────────────────────

/// `name version`, one per line. pacman and brew both speak this.
fn parse_space_pairs(out: &str) -> BTreeMap<String, String> {
    out.lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let name = it.next()?;
            // brew prints every installed version; the first is enough to
            // tell "changed" from "not changed".
            Some((name.to_string(), it.next().unwrap_or("").to_string()))
        })
        .collect()
}

/// `name<TAB>version<TAB>status`, keeping only `install ok installed`.
fn parse_dpkg(out: &str) -> BTreeMap<String, String> {
    out.lines()
        .filter_map(|l| {
            let mut it = l.split('\t');
            let name = it.next()?;
            let version = it.next().unwrap_or("");
            let status = it.next().unwrap_or("");
            (status.trim() == "install ok installed")
                .then(|| (name.to_string(), version.to_string()))
        })
        .collect()
}

/// winget prints a fixed-width table. Splitting on whitespace would take the
/// second *word* of the name — "Google Chrome" and "Visual Studio Code" are
/// the common case — so the columns are cut at the offsets the header gives.
/// Untested: there is no Windows box in this loop, and plan.md says so.
fn parse_winget(out: &str) -> BTreeMap<String, String> {
    let mut lines = out.lines();
    let Some(header) = lines.find(|l| l.contains("Id") && l.contains("Version")) else {
        return BTreeMap::new();
    };
    let (Some(id_at), Some(version_at)) = (header.find("Id"), header.find("Version")) else {
        return BTreeMap::new();
    };
    let cut = |l: &str, from: usize, to: Option<usize>| -> String {
        let chars: Vec<char> = l.chars().collect();
        if from >= chars.len() {
            return String::new();
        }
        let end = to.unwrap_or(chars.len()).min(chars.len());
        chars[from..end.max(from)].iter().collect::<String>().trim().to_string()
    };
    lines
        .skip_while(|l| l.starts_with('-'))
        .filter_map(|l| {
            let id = cut(l, id_at, Some(version_at));
            (!id.is_empty()).then(|| (id, cut(l, version_at, None)))
        })
        .collect()
}

// ── parsing ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spec {
    pub names: Vec<String>,
    pub state: State,
    pub manager: &'static Manager,
    pub cask: bool,
    pub update_cache: bool,
}

pub fn parse(step: &Step<'_>, engine: &Engine, ctx: &Value, raw: bool) -> Result<Option<Spec>> {
    let body = step.body;
    let render = |src: &str| -> Option<String> {
        if raw { Some(src.to_string()) } else { engine.render(src, ctx).ok() }
    };

    let names = match (body.get("name"), body.get("names")) {
        (Some(_), Some(n)) => {
            return Err(n.err("`name` and `names` are mutually exclusive"));
        }
        (Some(n), None) => {
            let Some(one) = render(n.as_str()?) else { return Ok(None) };
            vec![one]
        }
        (None, Some(n)) => {
            let Ok(value) = engine.render_node(n, ctx) else { return Ok(None) };
            match value.try_iter() {
                Ok(it) => it.map(|v| v.to_string()).collect(),
                Err(_) => return Err(n.err("`names` is a list of package names")),
            }
        }
        (None, None) => return Err(body.err("`pkg` requires `name` or `names`")),
    };
    if names.is_empty() {
        return Err(body.err("`pkg` names no packages"));
    }

    let state = match body.get("state") {
        Some(n) => {
            let Some(text) = render(n.as_str()?) else { return Ok(None) };
            match text.as_str() {
                "present" => State::Present,
                "absent" => State::Absent,
                "latest" => State::Latest,
                other => {
                    return Err(n
                        .err(format!("unknown package state `{other}`"))
                        .with_note("one of: present, absent, latest"));
                }
            }
        }
        None => State::Present,
    };

    let cask = body.get("cask").and_then(|n| n.as_bool().ok()).unwrap_or(false);
    let update_cache = body.get("update_cache").and_then(|n| n.as_bool().ok()).unwrap_or(false);
    let sudo = step.mods.sudo.map(|n| n.as_bool().unwrap_or(false)).unwrap_or(false);

    let manager = match body.get("manager") {
        Some(n) => {
            let Some(text) = render(n.as_str()?) else { return Ok(None) };
            match find(&text) {
                Some(m) => m,
                None => {
                    let known: Vec<&str> = MANAGERS.iter().map(|m| m.name).collect();
                    return Err(n
                        .err(format!("unknown package manager `{text}`"))
                        .with_note(format!("one of: {}", known.join(", "))));
                }
            }
        }
        // Spec §6.5: the first of the table on PATH, skipping the ones a plan
        // has to ask for by name.
        None => match MANAGERS
            .iter()
            .find(|m| !m.never_default && which::which(m.bin).is_ok())
        {
            Some(m) => m,
            None => {
                return Err(body
                    .err("no package manager found")
                    .with_note("looked for: pacman, apt-get, brew, winget"));
            }
        },
    };

    if manager.rejects_root && sudo {
        return Err(step
            .at
            .err(format!("`{}` must not run as root", manager.name))
            .with_note(format!("{} refuses to be run under sudo", manager.name)));
    }
    if cask && manager.cask_flag.is_none() {
        return Err(body
            .err(format!("`cask` does not apply to `{}`", manager.name))
            .with_note("casks are a brew concept"));
    }

    Ok(Some(Spec { names, state, manager, cask, update_cache }))
}

// ── the verdict, which asks the row and never switches on it ─────────────

impl Spec {
    pub fn plan(&self, ctx: &Ctx<'_>) -> Effect {
        self.reach(ctx, false)
    }

    pub fn apply(&self, ctx: &Ctx<'_>) -> Effect {
        self.reach(ctx, true)
    }

    fn reach(&self, ctx: &Ctx<'_>, act: bool) -> Effect {
        let m = self.manager;
        if which::which(m.bin).is_err() {
            return Effect::fail(format!("`{}` is not on PATH", m.bin));
        }
        // No unprobed guard here, unlike `file`: the query reads a local
        // database and never escalates, plan never mutates, and
        // `update_cache` never runs under plan. There is nothing for root to
        // protect, so plan answers even with a cold credential — the same as
        // `service`, which probes this situation identically.

        let before = match self.installed(ctx) {
            Ok(map) => map,
            Err(bad) => return bad,
        };

        match self.state {
            State::Present => {
                let missing = self.subset(&before, false);
                if missing.is_empty() {
                    return Effect::Ok;
                }
                if act && let Err(bad) = self.run_on(ctx, m.install, &missing) {
                    return bad;
                }
                Effect::Changed(Some(format!("install {}", missing.join(" "))))
            }
            State::Absent => {
                let present = self.subset(&before, true);
                if present.is_empty() {
                    return Effect::Ok;
                }
                if act && let Err(bad) = self.run_on(ctx, m.remove, &present) {
                    return bad;
                }
                Effect::Changed(Some(format!("remove {}", present.join(" "))))
            }
            State::Latest => {
                let missing = self.subset(&before, false);
                if !act {
                    // Whether a newer version exists is not a question the
                    // local database answers, so plan says so rather than
                    // guessing. A missing package is knowable, and reported.
                    return if missing.is_empty() {
                        Effect::Unknown
                    } else {
                        Effect::Changed(Some(format!("install {}", missing.join(" "))))
                    };
                }
                // Install what is absent before upgrading what is not.
                // `apt-get install --only-upgrade` silently skips a package
                // that is not installed, and brew and winget error on one, so
                // upgrading the whole list would return ok with the missing
                // package still missing — plan saying `install X` and apply
                // quietly not doing it.
                if !missing.is_empty() && let Err(bad) = self.run_on(ctx, m.install, &missing) {
                    return bad;
                }
                let held: Vec<String> =
                    self.names.iter().filter(|n| !missing.contains(n)).cloned().collect();
                if !held.is_empty() && let Err(bad) = self.run_on(ctx, m.upgrade, &held) {
                    return bad;
                }
                let after = match self.installed(ctx) {
                    Ok(map) => map,
                    Err(bad) => return bad,
                };
                let moved: Vec<String> = self
                    .names
                    .iter()
                    .filter(|n| before.get(key(n)) != after.get(key(n)))
                    .cloned()
                    .collect();
                if moved.is_empty() {
                    Effect::Ok
                } else {
                    Effect::Changed(Some(format!("upgrade {}", moved.join(" "))))
                }
            }
        }
    }

    /// One query for the whole set (spec §6.5), not one per package.
    fn installed(&self, ctx: &Ctx<'_>) -> std::result::Result<BTreeMap<String, String>, Effect> {
        let m = self.manager;
        let argv = if self.cask { m.query_cask.unwrap_or(m.query) } else { m.query };
        // A query reads a local database and needs no privilege, so it never
        // escalates — the same rule the gates and `service` probes follow.
        let got = ctx
            .exec(argv, false)
            .map_err(|e| Effect::fail(format!("cannot run {}: {e}", argv[0])))?;
        if let Some(bad) = ctx.stopped(&got) {
            return Err(bad);
        }
        Ok((m.parse)(&String::from_utf8_lossy(&got.stdout)))
    }

    fn subset(&self, installed: &BTreeMap<String, String>, want_present: bool) -> Vec<String> {
        self.names
            .iter()
            .filter(|n| installed.contains_key(key(n)) == want_present)
            .cloned()
            .collect()
    }

    /// One call for the whole subset (spec §6.5), with the cache refreshed
    /// first only when there is something to do.
    fn run_on(
        &self,
        ctx: &Ctx<'_>,
        verb: &'static [&'static str],
        names: &[String],
    ) -> std::result::Result<(), Effect> {
        let m = self.manager;
        if self.update_cache && let Some(refresh) = m.refresh {
            let argv: Vec<&str> = refresh.to_vec();
            self.exec(ctx, &argv)?;
        }
        let batches: Vec<&[String]> = if m.one_per_call {
            names.chunks(1).collect()
        } else {
            vec![names]
        };
        for batch in batches {
            let mut argv: Vec<&str> = verb.to_vec();
            if self.cask && let Some(flag) = m.cask_flag {
                argv.push(flag);
            }
            argv.extend(batch.iter().map(String::as_str));
            self.exec(ctx, &argv)?;
        }
        Ok(())
    }

    fn exec(&self, ctx: &Ctx<'_>, argv: &[&str]) -> std::result::Result<(), Effect> {
        match ctx.perform(argv, ctx.sudo) {
            Some(bad) => Err(bad),
            None => Ok(()),
        }
    }
}

/// The name a query's output is keyed by.
///
/// brew accepts a tap-qualified name — `hashicorp/tap/packer` — and then
/// lists it as plain `packer`, so a step naming the qualified form would
/// never find it installed and would reinstall on every apply. Only brew puts
/// a slash in a package name, so taking the last path segment is safe for
/// every row and needs no field to say which. The *install* argv still gets
/// the name as written; only the lookup is normalised.
fn key(name: &str) -> &str {
    name.rsplit('/').next().unwrap_or(name)
}
