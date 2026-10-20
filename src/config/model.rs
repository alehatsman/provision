//! The step model, parsed from spanned YAML. Spec §3 and §4.
//!
//! Parsing is structural only: it proves a step has exactly one action key and
//! no unknown keys, and it keeps every field as a spanned node so that a later
//! render or type error still points at the right line. Nothing is templated
//! here — that needs a scope, which arrives at expansion.

use crate::error::Result;
use crate::template::Engine;
use crate::yaml::N;
use minijinja::Value;
use std::time::Duration;

pub(crate) const STRUCTURAL_KEYS: &[&str] = &["vars", "vars_file", "import", "use"];
/// What one step modifier is, for `provision list --json` (D25).
///
/// The envelope is what makes a step *correct* rather than merely
/// parseable, and until this table existed nothing described it: `list
/// <action>` covered the ten actions and the sixteen keys every step may
/// carry were a bare name list. `applies` is the word a reader needs most,
/// because four of these are only legal on one kind of step.
pub(crate) struct ModifierDoc {
    pub name: &'static str,
    /// Where the modifier is **meaningful**, as prose for a reader. It is
    /// deliberately *not* a rule the parser enforces: only structural steps
    /// reject modifiers they cannot honour (`check_modifiers`), and `retry`
    /// is the one action-step key rejected on the typed actions; every other
    /// modifier in this table is accepted on every action step. Several entries are
    /// narrower than reality on purpose -- `changed_when` is documented for
    /// command steps and a `file` step honours it too.
    ///
    /// Written down because this is a published contract (D25) and an
    /// unenforced claim an agent cannot distinguish from an enforced one is
    /// worse than no claim. §4 says the same thing in the same words.
    pub applies: &'static str,
    pub note: &'static str,
}

const fn m(name: &'static str, applies: &'static str, note: &'static str) -> ModifierDoc {
    ModifierDoc {
        name,
        applies,
        note,
    }
}

/// Every key a step may carry besides its one action or structural key
/// (spec §4). The unknown-key check reads this table, so a key that is not
/// here is not accepted, and a key here that the walk ignores is a bug this
/// table makes visible.
pub(crate) static MODIFIERS: &[ModifierDoc] = &[
    m("name", "any step", "what the step line says; templated"),
    m(
        "when",
        "any step",
        "an expression; false skips the step and, on a `use`/`import`, its whole subtree",
    ),
    m("unless", "any action", "a command; exit 0 skips the step"),
    m("creates", "any action", "a path; existing skips the step"),
    m(
        "sudo",
        "any action",
        "run as root; templated, and a sole expression must yield a bool. it cannot read a register: privilege is known before the run starts",
    ),
    m(
        "timeout",
        "any action",
        "per attempt, not per step: `30s`, `5m`, `1h`. bounds every command a typed action spawns, not only `shell`/`cmd`",
    ),
    m(
        "retry",
        "`shell`, `cmd`, `assert`",
        "a mapping: `{attempts, delay}`; rejected on a typed action, which has no retry loop",
    ),
    m(
        "env",
        "action steps",
        "a mapping added to the command's environment; values templated",
    ),
    m("cwd", "command steps", "the working directory; templated"),
    m(
        "tags",
        "any step",
        "a name or a list; `always` survives every filter (D7)",
    ),
    m(
        "register",
        "action steps",
        "bind this step's result to a name for a later `when`",
    ),
    m(
        "changed_when",
        "any action",
        "an expression deciding `changed`; also satisfies the gate. most useful on a command step, honoured on every action",
    ),
    m(
        "failed_when",
        "any action",
        "an expression deciding `failed`; `false` is `ignore_errors`",
    ),
    m(
        "raw",
        "any action",
        "do not render the action body as a template; written for `shell`, honoured by every action",
    ),
    m(
        "props",
        "`use` only",
        "the arguments this call site passes the component, checked against the component root's `props:` schema -- which is a component key, not a step key, and is not in this table",
    ),
    m(
        "optional",
        "`vars_file` only",
        "a missing file is not an error",
    ),
];

/// The modifier names, for the unknown-key check.
pub(crate) fn modifier_keys() -> impl Iterator<Item = &'static str> {
    MODIFIERS.iter().map(|m| m.name)
}

/// Every key a step may carry, for the unknown-key check.
pub(crate) fn all_step_keys() -> Vec<&'static str> {
    action_keys()
        .chain(STRUCTURAL_KEYS.iter().copied())
        .chain(modifier_keys())
        .collect()
}

/// One field of an action's body (spec §6).
///
/// `need` is the word `provision list <action>` prints and nothing branches
/// on: "required", "optional", or the one-of group a field belongs to. The
/// rule itself lives in the action's own `parse`, which is the only place
/// that can enforce it; this is how it reads out loud.
pub(crate) struct Field {
    pub name: &'static str,
    pub need: &'static str,
    pub note: &'static str,
}

/// Where a step's action writes, for the two checks that read it: D18's
/// deferral of a later `file` `src`, and the unrendered-`{{` check on a path
/// provision reads itself.
#[derive(Clone, Copy)]
pub(crate) enum Dest {
    /// The action names no path of its own. `shell` and `cmd` are the reason
    /// a plan defers every later `src`: they declare nothing.
    None,
    /// The body field naming the path the action always writes.
    Path(&'static str),
    /// As `Path`, but `state: absent` removes it, which is no output.
    PathUnlessAbsent(&'static str),
}

impl Dest {
    /// The field to check for an unrendered `{{`, whatever the state.
    pub(crate) fn field(self) -> Option<&'static str> {
        match self {
            Dest::None => None,
            Dest::Path(f) | Dest::PathUnlessAbsent(f) => Some(f),
        }
    }
}

/// What one action is, in a line, for `provision list <action>`.
///
/// `typed` and `dest` have no default on purpose: a new row cannot be written
/// without deciding both, and a decision nobody made is how an action ends up
/// silently missing from the runner's typed path or from D18.
pub(crate) struct ActionDoc {
    /// An action that inspects and changes state itself rather than reporting
    /// through an exit code. It never reaches the runner's argv path.
    pub typed: bool,
    pub dest: Dest,
    pub summary: &'static str,
    /// One step that uses the action, as YAML to put under `- name:`. The
    /// smoke test runs every one through `validate` and `plan`, so a row
    /// without a working example fails the build.
    pub example: &'static str,
    /// The body fields, or empty for an action whose body is not a mapping.
    pub fields: &'static [Field],
    /// What the body is when it is not a mapping. `None` where it is one.
    pub shape: Option<&'static str>,
}

const fn f(name: &'static str, need: &'static str, note: &'static str) -> Field {
    Field { name, need, note }
}

/// Every builtin action's body, in one table (spec §6, §8).
///
/// The unknown-key check and `provision list <action>` both read it, because
/// a printed key list kept by hand beside the one the validator enforces is
/// how a tool grows a help text that lies. The summaries and glosses are §6's
/// own words.
static ACTIONS: &[(&str, ActionDoc)] = &[
    (
        "shell",
        ActionDoc {
            example: "shell: \"true\"",
            typed: false,
            dest: Dest::None,
            summary: "Run a script through an interpreter.",
            fields: &[
                f("script", "required", "the script to run"),
                f(
                    "interpreter",
                    "optional",
                    "bash (default on unix) | sh | zsh | powershell (default on windows) | pwsh",
                ),
                f("login", "optional", "run the interpreter as a login shell"),
            ],
            shape: Some("a script string: `shell: |`, with the long form below"),
        },
    ),
    (
        "cmd",
        ActionDoc {
            example: "cmd: [echo, hello]",
            typed: false,
            dest: Dest::None,
            summary: "Run an argv without a shell.",
            fields: &[],
            shape: Some("a list of arguments: `cmd: [mv, src, dest]`"),
        },
    ),
    (
        "file",
        ActionDoc {
            example: "file: {path: ./out/dir, state: dir}",
            typed: true,
            dest: Dest::PathUnlessAbsent("path"),
            summary: "Ensure a filesystem entry.",
            fields: &[
                f("path", "required", "the entry to ensure"),
                f(
                    "state",
                    "optional",
                    "file | dir | absent | link (default: file)",
                ),
                f(
                    "content",
                    "one of content/src",
                    "state=file only: the bytes",
                ),
                f(
                    "src",
                    "one of content/src",
                    "state=file: a file to copy; state=link: the link target",
                ),
                f("mode", "optional", "octal, quoted: \"0644\""),
                f("owner", "optional", "requires sudo"),
                f("group", "optional", "requires sudo"),
                f(
                    "force",
                    "optional",
                    "replace a file with a link, or remove a non-empty directory",
                ),
            ],
            shape: None,
        },
    ),
    (
        "json",
        ActionDoc {
            example: "json: {path: ./out/settings.json, content: {a: 1}}",
            typed: true,
            dest: Dest::Path("path"),
            summary: "Ensure a JSON file contains the declared keys, leaving every other key alone.",
            fields: &[
                f("path", "required", "the JSON file to merge into"),
                f(
                    "content",
                    "one of content/src",
                    "the keys this plan owns, as a mapping",
                ),
                f(
                    "src",
                    "one of content/src",
                    "a .json file holding those keys",
                ),
                f("mode", "optional", "octal, quoted: \"0644\""),
            ],
            shape: None,
        },
    ),
    (
        "template",
        ActionDoc {
            example: "template: {src: ./t.j2, dest: ./out/rendered}",
            typed: true,
            dest: Dest::Path("dest"),
            summary: "Render a `.j2` file and ensure `dest` matches, with `file` semantics.",
            fields: &[
                f(
                    "src",
                    "required",
                    "a file, or a directory to render as a tree",
                ),
                f("dest", "required", "the output file, or the tree root"),
                f(
                    "mode",
                    "optional",
                    "as `file` takes it; every rendered file",
                ),
                f("owner", "optional", "requires sudo"),
                f("group", "optional", "requires sudo"),
            ],
            shape: None,
        },
    ),
    (
        "pkg",
        ActionDoc {
            example: "pkg: {name: git, manager: apt}",
            typed: true,
            dest: Dest::None,
            summary: "Ensure packages are present or absent through one manager.",
            fields: &[
                f("name", "one of name/names", "a single package"),
                f("names", "one of name/names", "a list of packages"),
                f(
                    "state",
                    "optional",
                    "present | absent | latest (default: present)",
                ),
                f(
                    "version",
                    "optional",
                    "a `>=` floor, `present` only: `>=24`",
                ),
                f(
                    "manager",
                    "optional",
                    "apt | pacman | yay | brew | winget; default: first available",
                ),
                f("cask", "optional", "brew only"),
                f(
                    "adopt",
                    "optional",
                    "brew casks only; takes over an app already there",
                ),
                f(
                    "update_cache",
                    "optional",
                    "apt update / pacman -Sy / brew update before install",
                ),
            ],
            shape: None,
        },
    ),
    (
        "service",
        ActionDoc {
            example: "service: {name: sshd, state: started}",
            typed: true,
            dest: Dest::None,
            summary: "Ensure a service state under systemd (system or user) or launchd.",
            fields: &[
                f("name", "required", "the unit or service name"),
                f(
                    "state",
                    "one of state/enabled",
                    "started | stopped | restarted | reloaded",
                ),
                f("enabled", "one of state/enabled", "start at boot"),
                f("scope", "optional", "system | user (systemd only)"),
            ],
            shape: None,
        },
    ),
    (
        "assert",
        ActionDoc {
            example: "assert: {expr: \"true\"}",
            typed: false,
            dest: Dest::None,
            summary: "Fail the run if a condition does not hold. Never changes anything.",
            fields: &[
                f(
                    "command",
                    "one of command/expr",
                    "a shell command that exits 0",
                ),
                f(
                    "expr",
                    "one of command/expr",
                    "an expression over the scope, evaluated not rendered",
                ),
                f("msg", "optional", "what to print when it does not hold"),
            ],
            shape: None,
        },
    ),
    (
        "git",
        ActionDoc {
            example: "git: {repo: \"https://example.invalid/r.git\", dest: ./out/r}",
            typed: true,
            dest: Dest::Path("dest"),
            summary: "Ensure a checkout of a repository at a ref.",
            fields: &[
                f("repo", "required", "the URL to clone from"),
                f("dest", "required", "the directory the checkout lives in"),
                f(
                    "ref",
                    "optional",
                    "tag, sha, or branch; default: the remote's default branch",
                ),
                f(
                    "depth",
                    "optional",
                    "shallow; requires a `ref` naming a tag or a commit",
                ),
            ],
            shape: None,
        },
    ),
    (
        "download",
        ActionDoc {
            example: "download: {url: \"https://example.invalid/x\", dest: ./out/x, sha256: \"0000000000000000000000000000000000000000000000000000000000000000\"}",
            typed: true,
            dest: Dest::Path("dest"),
            summary: "Ensure a file fetched from a URL is at `dest`.",
            fields: &[
                f("url", "required", "what to fetch"),
                f("dest", "required", "where it goes"),
                f(
                    "sha256",
                    "optional",
                    "strongly recommended; the content's digest",
                ),
                f("mode", "optional", "as `file` takes it"),
                f("owner", "optional", "requires sudo"),
                f("group", "optional", "requires sudo"),
            ],
            shape: None,
        },
    ),
    (
        "defaults",
        ActionDoc {
            example: "defaults: {domain: com.apple.finder, key: ShowPathbar, type: bool, value: true}",
            typed: true,
            dest: Dest::None,
            summary: "Ensure a macOS preference key holds a value.",
            fields: &[
                f("domain", "required", "com.apple.finder, or NSGlobalDomain"),
                f("key", "required", "the preference key"),
                f("type", "required", "bool | int | float | string"),
                f("value", "required", "the value, in that type"),
                f("current_host", "optional", "`defaults -currentHost`"),
            ],
            shape: None,
        },
    ),
];

/// Every builtin, in table order, for `list --json`.
pub(crate) fn action_docs() -> impl Iterator<Item = (&'static str, &'static ActionDoc)> {
    ACTIONS.iter().map(|(n, d)| (*n, d))
}

/// The action keys, in table order.
pub(crate) fn action_keys() -> impl Iterator<Item = &'static str> {
    ACTIONS.iter().map(|(n, _)| *n)
}

pub(crate) fn action_doc(action: &str) -> Option<&'static ActionDoc> {
    ACTIONS.iter().find(|(n, _)| *n == action).map(|(_, d)| d)
}

/// The fields each action's long form accepts (spec §6), for the unknown-key
/// check. `cmd` is a sequence, not a map, and so has no fields.
pub(crate) fn action_body_keys(action: &str) -> Option<Vec<&'static str>> {
    let doc = action_doc(action)?;
    if doc.fields.is_empty() {
        return None;
    }
    Some(doc.fields.iter().map(|f| f.name).collect())
}

#[derive(Clone, Copy)]
pub(crate) struct Step<'a> {
    pub at: N<'a>,
    /// The one action or structural key this step carries.
    pub key: &'a str,
    pub body: N<'a>,
    pub mods: Mods<'a>,
}

impl Step<'_> {
    pub(crate) fn is_structural(&self) -> bool {
        STRUCTURAL_KEYS.contains(&self.key)
    }

    /// Used when `name` is absent or renders empty (spec §10).
    pub(crate) fn fallback_name(&self) -> String {
        let arg = match self.body.as_str() {
            Ok(s) => s.lines().next().unwrap_or("").trim().to_string(),
            Err(_) => self
                .body
                .keys()
                .ok()
                .and_then(|ks| ks.first().map(|(k, _)| k.to_string()))
                .or_else(|| {
                    self.body.as_seq().ok().and_then(|items| {
                        items
                            .first()
                            .and_then(|i| i.as_str().ok())
                            .map(String::from)
                    })
                })
                .unwrap_or_default(),
        };
        if arg.is_empty() {
            self.key.to_string()
        } else {
            let arg = truncate(&arg, 48);
            format!("{}: {arg}", self.key)
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max - 1).collect();
    format!("{head}…")
}

#[derive(Clone, Copy, Default)]
pub(crate) struct Mods<'a> {
    pub name: Option<N<'a>>,
    pub when: Option<N<'a>>,
    pub unless: Option<N<'a>>,
    pub creates: Option<N<'a>>,
    pub sudo: Option<N<'a>>,
    pub timeout: Option<N<'a>>,
    pub retry: Option<N<'a>>,
    pub env: Option<N<'a>>,
    pub cwd: Option<N<'a>>,
    pub tags: Option<N<'a>>,
    pub register: Option<N<'a>>,
    pub changed_when: Option<N<'a>>,
    pub failed_when: Option<N<'a>>,
    pub raw: Option<N<'a>>,
    pub props: Option<N<'a>>,
    pub optional: Option<N<'a>>,
}

/// Parse the list of steps at the top of a plan file.
///
/// A malformed step does not stop the others: the gate is that *every*
/// validation error carries a position, which means reporting all of them.
pub(crate) fn parse_steps(root: N<'_>) -> Result<(Vec<Step<'_>>, Vec<crate::error::Diag>)> {
    // The dropped cause is our own "expected a sequence" — the replacement
    // says the same thing and names the kind it actually found.
    #[expect(
        clippy::map_err_ignore,
        reason = "the replacement diagnostic restates the cause"
    )]
    let items = root.as_seq().map_err(|_| {
        root.err(format!("a plan is a list of steps, found {}", root.kind()))
            .with_note(
                "a component is a mapping with `props:` and `steps:` and is entered with `use:`",
            )
    })?;
    let mut steps = Vec::new();
    let mut errors = Vec::new();
    for item in items {
        match parse_step(item) {
            Ok(s) => steps.push(s),
            Err(d) => errors.push(d),
        }
    }
    Ok((steps, errors))
}

pub(crate) fn parse_step<'a>(at: N<'a>) -> Result<Step<'a>> {
    #[expect(
        clippy::map_err_ignore,
        reason = "the replacement diagnostic restates the cause"
    )]
    let keys = at
        .as_map()
        .map_err(|_| at.err(format!("a step is a mapping, found {}", at.kind())))
        .and_then(|_| at.keys())?;

    at.deny_unknown_keys(&all_step_keys(), "a step")?;

    let subjects: Vec<(&str, N<'a>)> = keys
        .iter()
        .copied()
        .filter(|(k, _)| action_doc(k).is_some() || STRUCTURAL_KEYS.contains(k))
        .collect();

    let (key, _key_at) = match subjects.len() {
        1 if let Some(only) = subjects.first() => *only,
        0 => {
            return Err(at.err("step has no action").with_note(format!(
                "expected one of: {}",
                all_subject_keys().join(", ")
            )));
        }
        _ => {
            let names: Vec<&str> = subjects.iter().map(|(k, _)| *k).collect();
            // Point at the second one: the first is likely what was meant.
            let second = subjects.get(1).map_or(at, |(_, n)| *n);
            return Err(second
                .err(format!(
                    "step has {} action keys: {}",
                    names.len(),
                    names.join(", ")
                ))
                .with_note("a step carries exactly one action or structural key"));
        }
    };

    let body = at.require(key)?;
    let mut mods = Mods::default();
    for (name, key_node) in keys {
        if !modifier_keys().any(|k| k == name) {
            continue;
        }
        let v = at.require(name)?;
        let _ = key_node;
        match name {
            "name" => mods.name = Some(v),
            "when" => mods.when = Some(v),
            "unless" => mods.unless = Some(v),
            "creates" => mods.creates = Some(v),
            "sudo" => mods.sudo = Some(v),
            "timeout" => mods.timeout = Some(v),
            "retry" => mods.retry = Some(v),
            "env" => mods.env = Some(v),
            "cwd" => mods.cwd = Some(v),
            "tags" => mods.tags = Some(v),
            "register" => mods.register = Some(v),
            "changed_when" => mods.changed_when = Some(v),
            "failed_when" => mods.failed_when = Some(v),
            "raw" => mods.raw = Some(v),
            "props" => mods.props = Some(v),
            "optional" => mods.optional = Some(v),
            _ => unreachable!("modifier list and match are the same set"),
        }
    }

    Ok(Step {
        at,
        key,
        body,
        mods,
    })
}

fn all_subject_keys() -> Vec<&'static str> {
    action_keys()
        .chain(STRUCTURAL_KEYS.iter().copied())
        .collect()
}

// ── modifier types ────────────────────────────────────────────────────────

/// Why a duration would not read. `TooBig` gets its own note, because "use a
/// number and a unit" is misleading advice for input that is exactly a number
/// and a unit.
pub(crate) enum BadDuration {
    /// Not `<digits><unit>` at all.
    Shape,
    /// The right shape, more seconds than one can hold. Carries the cause.
    TooBig(String),
}

impl BadDuration {
    pub(crate) fn note(&self) -> &'static str {
        match self {
            BadDuration::Shape => "use a number and a unit: 30s, 5m, 1h",
            BadDuration::TooBig(_) => "the largest a duration can be is 18446744073709551615s",
        }
    }

    /// Named after a colon when there is a cause worth naming.
    pub(crate) fn cause(&self) -> Option<&str> {
        match self {
            BadDuration::Shape => None,
            BadDuration::TooBig(c) => Some(c),
        }
    }
}

/// `30s`, `5m`, `1h`. Spec §4.
///
/// Split out for `--deadline` (§8), which has a string from the command line
/// and no YAML node to hang a position on. One grammar with two entry points
/// rather than two grammars: a `--deadline 5m` that meant something other
/// than a step's `timeout: 5m` would be a trap laid for the one reader who
/// noticed.
pub(crate) fn duration_from_str(s: &str) -> std::result::Result<Duration, BadDuration> {
    let s = s.trim();
    let split = s
        .find(|c: char| !c.is_ascii_digit())
        .ok_or(BadDuration::Shape)?;
    let (num, unit) = s.split_at(split);
    // `num` is ASCII digits by construction, so once it is non-empty the only
    // way this fails is overflow. An empty `num` is `soon`, which is a shape
    // problem and not something the overflow note would help with.
    let n: u64 = num.parse().map_err(|e: std::num::ParseIntError| {
        if num.is_empty() {
            BadDuration::Shape
        } else {
            BadDuration::TooBig(e.to_string())
        }
    })?;
    // `checked_mul`, not `*`: `1000000000h` wrapped silently in release and
    // panicked in debug, so the one duration nobody should be able to write
    // was the one that behaved differently per profile.
    let too_big =
        || BadDuration::TooBig(format!("{n}{unit} is more seconds than a duration holds"));
    let secs = match unit {
        "s" => n,
        "m" => n.checked_mul(60).ok_or_else(too_big)?,
        "h" => n.checked_mul(3600).ok_or_else(too_big)?,
        _ => return Err(BadDuration::Shape),
    };
    Ok(Duration::from_secs(secs))
}

/// The same grammar, positioned.
/// Is this node a template rather than a literal? Spec §3.4 says every string
/// value in a step is templated; `sudo`, `timeout` and `retry.attempts` are
/// typed rather than textual, so they are read as YAML and used to reject a
/// template outright — the error blaming the *value* for not being a bool or a
/// duration, rather than saying the field would not render it (#35).
///
/// They are templated now, which makes §3.4's claim true instead of retracting
/// it. That costs one thing: the type can no longer be checked at parse time,
/// because the value does not exist until there is a scope. So parsing defers
/// exactly the nodes that hold a template and keeps checking every literal,
/// which is what almost every step writes.
///
/// A template is any Jinja delimiter, not just `{{`: a statement block is one
/// too, and `timeout: "{% if facts.os == 'darwin' %}5m{% else %}30s{% endif %}"`
/// is the cross-platform shape §3.4 exists for — read as a literal, it went to
/// the duration parser as raw text (#61). The wider test gives up no check that
/// could have passed, since no duration, bool or whole number literal holds a
/// delimiter.
pub(crate) fn is_template(at: N<'_>) -> bool {
    at.as_str()
        .is_ok_and(|s| s.contains("{{") || s.contains("{%") || s.contains("{#"))
}

/// Render a typed modifier and read it as a bool. §3.4's sole-expression rule
/// applies, so `sudo: "{{ props.root }}"` yields the bool the prop holds and
/// not the string "true" — which is the whole point, since a step cannot be
/// half root.
pub(crate) fn mod_bool(at: N<'_>, engine: &Engine, ctx: &Value) -> Result<bool> {
    if !is_template(at) {
        return at.as_bool();
    }
    let src = at.as_str()?;
    let v = engine
        .render_field(src, ctx)
        .map_err(|e| at.err(format!("`{src}` could not be rendered: {e}")))?;
    // `is_true()` is not the test: it would take "no", 0 and [] for false and
    // a non-empty string for true, which is the silent coercion §3.4's typed
    // rule exists to avoid. The value has to *be* a bool — or spell one, since
    // a `{% … %}` block has no type to keep and can only render text (#61).
    if matches!(v.kind(), minijinja::value::ValueKind::Bool) {
        return Ok(v.is_true());
    }
    match v.as_str().map(str::trim) {
        Some("true") => Ok(true),
        Some("false") => Ok(false),
        _ => Err(at
            .err(format!(
                "expected true or false, `{src}` rendered to {}",
                describe_kind(&v)
            ))
            .with_note(concat!(
                "a sole `{{ expr }}` keeps its type, so point it at a bool; ",
                "any other template must render exactly `true` or `false`"
            ))),
    }
}

/// The same for a field read as text and then parsed: `timeout` and
/// `retry.delay` take §4's duration grammar, `retry.attempts` a whole number.
/// Rendered with `render`, not `render_field`: the result is parsed from text
/// either way, and a duration is not a minijinja type.
fn rendered_text(at: N<'_>, engine: &Engine, ctx: &Value) -> Result<String> {
    let src = at.as_str()?;
    engine
        .render(src, ctx)
        .map_err(|e| at.err(format!("`{src}` could not be rendered: {e}")))
}

pub(crate) fn mod_duration(at: N<'_>, engine: &Engine, ctx: &Value) -> Result<Duration> {
    if !is_template(at) {
        return parse_duration(at);
    }
    let text = rendered_text(at, engine, ctx)?;
    duration_from_str(&text).map_err(|e| {
        at.err(format!("`{text}` is not a duration"))
            .with_note(e.note())
    })
}

fn describe_kind(v: &Value) -> &'static str {
    use minijinja::value::ValueKind as K;
    match v.kind() {
        K::String => "a string",
        K::Number => "a number",
        K::Seq => "a list",
        K::Map => "a mapping",
        K::Undefined => "nothing",
        K::None => "null",
        _ => "something else",
    }
}

pub(crate) fn parse_duration(at: N<'_>) -> Result<Duration> {
    let raw = at.as_scalar_string()?;
    let s = raw.trim();
    duration_from_str(s).map_err(|e| {
        let msg = match e.cause() {
            Some(cause) => format!("`{s}` is not a duration: {cause}"),
            None => format!("`{s}` is not a duration"),
        };
        at.err(msg).with_note(e.note())
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Retry {
    pub attempts: u32,
    pub delay: Duration,
}

pub(crate) fn parse_retry(at: N<'_>) -> Result<Retry> {
    parse_retry_with(at, None)
}

/// `retry` with a scope, so `attempts` and `delay` may be templates (#35).
/// `None` is parse time, where a template is deferred rather than read.
pub(crate) fn mod_retry(at: N<'_>, engine: &Engine, ctx: &Value) -> Result<Retry> {
    parse_retry_with(at, Some((engine, ctx)))
}

fn parse_retry_with(at: N<'_>, scope: Option<(&Engine, &Value)>) -> Result<Retry> {
    // Before `deny_unknown_keys`, which asks the node for its map and reports
    // `expected a mapping, found an integer` — naming neither the key that
    // was wrong nor the shape it wanted. `retry: 3` is the obvious thing to
    // write, so it is the one that has to answer for itself.
    if at.as_map().is_err() {
        return Err(at
            .err(format!("`retry` is a mapping, found {}", at.kind()))
            .with_note("retry: { attempts: 3, delay: 2s } — `delay` is optional"));
    }
    at.deny_unknown_keys(&["attempts", "delay"], "`retry`")?;
    let attempts_at = at.require("attempts")?;
    let attempts = match (is_template(attempts_at), scope) {
        // A template with no scope yet: parse time. Checked when rendered.
        (true, None) => 1,
        (true, Some((engine, ctx))) => {
            let text = rendered_text(attempts_at, engine, ctx)?;
            text.parse()
                .map_err(|e| attempts_at.err(format!("`attempts` must be a whole number: {e}")))?
        }
        (false, _) => attempts_at
            .as_scalar_string()?
            .parse()
            .map_err(|e| attempts_at.err(format!("`attempts` must be a whole number: {e}")))?,
    };
    if attempts < 1 {
        return Err(attempts_at.err("`attempts` must be at least 1"));
    }
    let delay = match at.get("delay") {
        Some(d) => match (is_template(d), scope) {
            (true, None) => Duration::ZERO,
            (true, Some((engine, ctx))) => mod_duration(d, engine, ctx)?,
            (false, _) => parse_duration(d)?,
        },
        None => Duration::ZERO,
    };
    Ok(Retry { attempts, delay })
}

/// Check the modifier combinations that are wrong regardless of scope. The
/// rest wait for expansion, when values are rendered.
pub(crate) fn check_modifiers(step: &Step<'_>) -> Result<()> {
    let m = &step.mods;

    // A template has no value until there is a scope, so its type is checked
    // when it is rendered (see `mod_bool` / `mod_duration`) and not here.
    // Every literal is still checked at parse time, which is almost all of
    // them.
    if let Some(n) = m.timeout.filter(|n| !is_template(*n)) {
        parse_duration(n)?;
    }
    if let Some(n) = m.retry {
        parse_retry(n)?;
    }
    if let Some(n) = m.sudo.filter(|n| !is_template(*n)) {
        n.as_bool()?;
    }
    if let Some(n) = m.raw {
        n.as_bool()?;
    }
    if let Some(n) = m.optional {
        n.as_bool()?;
    }
    if let Some(n) = m.tags {
        n.as_str_or_seq()?;
    }
    if let Some(n) = m.env {
        n.as_map()?;
    }

    if let Some(n) = m.register {
        let name = n.as_str()?;
        if !is_identifier(name) {
            return Err(n
                .err(format!("`register: {name}` is not a valid identifier"))
                .with_note("letters, digits and underscore, not starting with a digit"));
        }
    }

    // `props` belongs to `use`; `optional` belongs to `vars_file`. Silently
    // ignoring a misplaced modifier is how a typo becomes a two-hour debug.
    if let Some(n) = m.props.filter(|_| step.key != "use") {
        return Err(n.err(format!(
            "`props` is only valid on `use`, not on `{}`",
            step.key
        )));
    }
    if let Some(n) = m.optional.filter(|_| step.key != "vars_file") {
        return Err(n.err(format!(
            "`optional` is only valid on `vars_file`, not on `{}`",
            step.key
        )));
    }

    // The same rule, for every modifier a structural step cannot honour.
    // `use` and `import` build a scope and walk it; they never reach
    // `do_action`, so none of these is ever read, and all of them were
    // accepted and dropped on the floor.
    //
    // `register` is the sharpest: D5 names it as the replacement for
    // handlers, so `register` on a `use` is the prescribed recipe, silently
    // doing nothing across a component boundary and surfacing one step later
    // as an undefined variable in the `when` that reads it. Worse, it is not
    // even consistently nothing — a *skipped* `use` does bind, because the
    // skip paths run `bind_register` and the success path does not.
    //
    // `when` and `tags` are the two that do propagate, and still do: they
    // gate the subtree before the walk descends, which is a different thing
    // from being handed to a command a structural step never runs. That is
    // also the note `unless` and `creates` get, because a reader reaching
    // for a gate on a `use` wants `when` and does not know it yet.
    if step.is_structural() {
        let gate = "`when:` gates a structural step and its whole subtree";
        let on_steps = "put it on the steps that run commands";
        for (node, name, note) in [
            (m.unless, "unless", gate),
            (m.creates, "creates", gate),
            (m.changed_when, "changed_when", gate),
            (m.failed_when, "failed_when", gate),
            (m.env, "env", on_steps),
            (
                m.register,
                "register",
                "register the step inside the component whose result you want",
            ),
            (m.cwd, "cwd", on_steps),
            (m.sudo, "sudo", on_steps),
            (m.timeout, "timeout", on_steps),
            (m.retry, "retry", on_steps),
            (m.raw, "raw", on_steps),
        ] {
            if let Some(n) = node {
                return Err(n
                    .err(format!("`{name}` has no effect on a `{}`", step.key))
                    .with_note(note));
            }
        }
    }

    // A typed action returns before the runner builds its retry loop, so the
    // key validated and then did nothing.
    if let Some(n) = m.retry.filter(|_| {
        action_doc(step.key).is_some() && !matches!(step.key, "shell" | "cmd" | "assert")
    }) {
        return Err(n
            .err(format!("`retry` has no effect on a `{}`", step.key))
            .with_note("only `shell`, `cmd` and `assert` steps retry"));
    }

    // Spec §4: sudo is root or nothing, and Windows has no sudo (D8).
    if let Some(n) = m
        .sudo
        .filter(|n| cfg!(target_os = "windows") && n.as_bool().unwrap_or(false))
    {
        return Err(n
            .err("`sudo` is not supported on Windows")
            .with_note("run provision from an elevated PowerShell prompt instead"));
    }

    Ok(())
}

fn is_identifier(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with(|c: char| c.is_ascii_digit())
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The expression fields, which compile as expressions rather than templates.
/// Every modifier node that may hold a template, for a caller that needs to
/// know what a step reads. `register` and `tags` are excluded: both are names
/// the plan writes, not values it reads.
pub(crate) fn modifier_nodes<'a>(step: &Step<'a>) -> Vec<N<'a>> {
    [
        step.mods.name,
        step.mods.when,
        step.mods.unless,
        step.mods.creates,
        step.mods.sudo,
        step.mods.timeout,
        step.mods.retry,
        step.mods.env,
        step.mods.cwd,
        step.mods.changed_when,
        step.mods.failed_when,
        step.mods.props,
    ]
    .into_iter()
    .flatten()
    .collect()
}

pub(crate) fn expression_fields<'a>(step: &Step<'a>) -> Vec<N<'a>> {
    [
        step.mods.when,
        step.mods.changed_when,
        step.mods.failed_when,
    ]
    .into_iter()
    .flatten()
    .collect()
}
