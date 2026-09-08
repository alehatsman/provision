//! `file` — ensure a filesystem entry. Spec §6.3.
//!
//! Two rules shape everything here. Reading obeys `sudo` as much as writing
//! does, because a target the step needs root to write is usually one it
//! needs root to read. And the atomic-write staging directory depends on
//! which path is taken: without sudo the temp file must sit next to the
//! destination, because rename is only atomic within one filesystem; with
//! sudo it must not, because being unable to create a file next to the
//! destination is the whole reason the step said sudo.

use super::{Ctx, Effect};
use crate::config::model::Step;
use crate::error::Result;
use crate::template::{Engine, expanduser};
use crate::yaml::N;
use minijinja::Value;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum State {
    File,
    Dir,
    Link,
    Absent,
}

impl State {
    fn parse(name: &str, at: N<'_>) -> Result<State> {
        Ok(match name {
            "file" => State::File,
            "dir" => State::Dir,
            "link" => State::Link,
            "absent" => State::Absent,
            other => {
                return Err(at
                    .err(format!("unknown file state `{other}`"))
                    .with_note("one of: file, dir, absent, link"));
            }
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Spec {
    pub path: String,
    pub state: State,
    /// The bytes the target must hold, resolved at parse time from `content`
    /// or by reading `src`, so probe and apply compare the same thing.
    pub content: Option<Vec<u8>>,
    /// `state: link` only: the target, stored exactly as written. No
    /// canonicalisation, so a relative link stays relative.
    pub link_to: Option<String>,
    pub mode: Option<u32>,
    pub owner: Option<String>,
    pub group: Option<String>,
    pub force: bool,
    /// Create the destination's leading directories, at `0755`. `file` does
    /// not; `template` in directory mode has to.
    pub make_parents: bool,
    /// What to call this file in a diff header. `template` in directory mode
    /// sets the path relative to `src`, which is what §6.4 asks a per-file
    /// diff to be headed by; everything else uses the destination.
    pub label: Option<String>,
}

/// The `mode`/`owner`/`group` trio. `template` takes all three with the same
/// meaning and the same sudo requirement (spec §6.4), so it parses them here
/// rather than growing a second set of rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Metadata {
    pub mode: Option<u32>,
    pub owner: Option<String>,
    pub group: Option<String>,
}

impl Metadata {
    /// A rendered template is a `file` whose content was computed.
    pub(crate) fn into_file(self, path: String, content: Vec<u8>, label: Option<String>) -> Spec {
        Spec {
            path,
            state: State::File,
            content: Some(content),
            link_to: None,
            mode: self.mode,
            owner: self.owner,
            group: self.group,
            force: false,
            make_parents: true,
            label,
        }
    }
}

/// Parse `mode`, `owner` and `group` from any action body that takes them.
pub(crate) fn parse_metadata(
    step: &Step<'_>,
    engine: &Engine,
    ctx: &Value,
    raw: bool,
) -> Result<Option<Metadata>> {
    let body = step.body;
    let render = |src: &str| -> Option<String> {
        if raw {
            Some(src.to_string())
        } else {
            engine.render(src, ctx).ok()
        }
    };

    let mode = match body.get("mode") {
        Some(n) => {
            // `mode: 0644` unquoted is a number, and which number depends on
            // whether the reader thinks it is YAML 1.1 (octal 420) or 1.2
            // (decimal 644). Neither is what was meant, so the error says to
            // quote it rather than picking one.
            let Ok(raw_text) = n.as_str() else {
                return Err(n.err("`mode` must be a quoted string").with_note(
                    "octal, quoted so YAML keeps the leading zero \
                     and does not read it as a number: \"0644\"",
                ));
            };
            let Some(text) = render(raw_text) else {
                return Ok(None);
            };
            Some(parse_mode(&text, n)?)
        }
        None => None,
    };

    // Spec §6.3: owner and group are independent, and both need sudo.
    let sudo = step.mods.sudo.is_some_and(|n| n.as_bool().unwrap_or(false));
    for (name, node) in [("owner", body.get("owner")), ("group", body.get("group"))] {
        if let Some(n) = node.filter(|_| !sudo) {
            return Err(n
                .err(format!("`{name}` requires `sudo: true`"))
                .with_note("only root may change a file's owner or group"));
        }
    }

    let field = |k: &str| body.get(k).and_then(|n| n.as_str().ok()).and_then(&render);
    Ok(Some(Metadata {
        mode,
        owner: field("owner"),
        group: field("group"),
    }))
}

// ── parsing ───────────────────────────────────────────────────────────────

pub(crate) fn parse(
    step: &Step<'_>,
    engine: &Engine,
    ctx: &Value,
    raw: bool,
) -> Result<Option<Spec>> {
    let body = step.body;
    let render = |src: &str| -> Option<String> {
        if raw {
            Some(src.to_string())
        } else {
            engine.render(src, ctx).ok()
        }
    };
    let Some(path_at) = body.get("path") else {
        return Err(body.err("`file` requires `path`"));
    };
    let Some(path) = render(path_at.as_str()?) else {
        return Ok(None);
    };

    let state = match body.get("state") {
        Some(n) => {
            let Some(name) = render(n.as_str()?) else {
                return Ok(None);
            };
            State::parse(&name, n)?
        }
        None => State::File,
    };

    let Some(meta) = parse_metadata(step, engine, ctx, raw)? else {
        return Ok(None);
    };
    let (mode, owner, group) = (meta.mode, meta.owner, meta.group);

    let src_at = body.get("src");
    let content_at = body.get("content");
    let force = body
        .get("force")
        .and_then(|n| n.as_bool().ok())
        .unwrap_or(false);

    let mut link_to = None;
    let mut content = None;

    match state {
        State::File => match (content_at, src_at) {
            (Some(_), Some(s)) => {
                return Err(s
                    .err("`content` and `src` are mutually exclusive")
                    .with_note("content is the bytes; src is a file to copy them from"));
            }
            (None, None) => {
                return Err(body
                    .err("`file` state file needs `content` or `src`")
                    .with_note("an empty file is `content: \"\"` — there is no touch"));
            }
            (Some(c), None) => {
                let Some(text) = render(c.as_str()?) else {
                    return Ok(None);
                };
                content = Some(text.into_bytes());
            }
            (None, Some(s)) => {
                let Some(rel) = render(s.as_str()?) else {
                    return Ok(None);
                };
                let from = crate::config::load::resolve(s.file, &expanduser(&rel));
                match std::fs::read(&from) {
                    Ok(bytes) => content = Some(bytes),
                    Err(e) => {
                        return Err(s.err(format!("cannot read {}: {e}", from.display())));
                    }
                }
            }
        },
        State::Link => {
            let Some(s) = src_at else {
                return Err(body
                    .err("`file` state link needs `src`")
                    .with_note("src is what the link points at"));
            };
            let Some(target) = render(s.as_str()?) else {
                return Ok(None);
            };
            // Spec §3: `~` expands in every path field the tool owns, and
            // `src` is one of them. "Stored as given" in §6.3 rules out
            // canonicalising — resolving `..`, following links, making a
            // relative target absolute — not this. Without it a link written
            // `~/.local/bin/moongit` never matches the absolute target on
            // disk and the step reports changed forever.
            link_to = Some(expanduser(&target));
            // Spec §6.3: a symlink has no mode of its own worth setting, and
            // silently ignoring one is how a plan grows a line that does
            // nothing for a year.
            for (name, node) in [
                ("mode", body.get("mode")),
                ("owner", body.get("owner")),
                ("group", body.get("group")),
            ] {
                if let Some(n) = node {
                    return Err(n
                        .err(format!("`{name}` does not apply to `state: link`"))
                        .with_note("set it on the file the link points at instead"));
                }
            }
        }
        State::Dir | State::Absent => {
            if let Some(n) = content_at {
                return Err(n.err(format!(
                    "`content` does not apply to `state: {}`",
                    state_name(state)
                )));
            }
        }
    }

    Ok(Some(Spec {
        path: expanduser(&path),
        state,
        content,
        link_to,
        mode,
        owner,
        group,
        force,
        make_parents: false,
        label: None,
    }))
}

fn state_name(s: State) -> &'static str {
    match s {
        State::File => "file",
        State::Dir => "dir",
        State::Link => "link",
        State::Absent => "absent",
    }
}

// ParseIntError says "invalid digit"; the note below says what a mode is.
#[expect(
    clippy::map_err_ignore,
    reason = "the replacement diagnostic restates the cause"
)]
fn parse_mode(text: &str, at: N<'_>) -> Result<u32> {
    u32::from_str_radix(text.trim(), 8).map_err(|_| {
        at.err(format!("`{text}` is not a file mode"))
            .with_note("octal, quoted so YAML keeps the leading zero: \"0644\"")
    })
}

// ── what is there now ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Missing,
    File,
    Dir,
    Link,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Meta {
    kind: Kind,
    mode: u32,
    owner: String,
    group: String,
}

impl Meta {
    fn missing() -> Meta {
        Meta {
            kind: Kind::Missing,
            mode: 0,
            owner: String::new(),
            group: String::new(),
        }
    }
}

/// Read the target's type, mode, owner and group — as root when the step said
/// sudo. `stat` does not follow symlinks by default on either platform, which
/// is what `state: link` needs.
fn stat(spec: &Spec, ctx: &Ctx<'_>) -> std::result::Result<Meta, String> {
    if ctx.reads_as_root() {
        let got = ctx
            .as_root(&["stat", stat_format(), &spec.path])
            .map_err(|e| format!("cannot run stat: {e}"))?;
        if got.rc != 0 {
            // stat fails the same way for "not there" and "cannot look", and
            // only the first is an answer. A parent directory that denies
            // even root is not a case worth inventing.
            return Ok(Meta::missing());
        }
        return Ok(parse_stat(&String::from_utf8_lossy(&got.stdout)));
    }

    match std::fs::symlink_metadata(&spec.path) {
        Ok(m) => Ok(Meta {
            kind: kind_of(&m),
            mode: mode_of(&m),
            owner: String::new(),
            group: String::new(),
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Meta::missing()),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => Err(format!(
            "cannot read {} ({e}); add `sudo: true` to read it as root",
            spec.path
        )),
        Err(e) => Err(format!("cannot read {}: {e}", spec.path)),
    }
}

#[cfg(target_os = "macos")]
fn stat_format() -> &'static str {
    "-f%HT|%Lp|%Su|%Sg"
}

#[cfg(not(target_os = "macos"))]
fn stat_format() -> &'static str {
    "-c%F|%a|%U|%G"
}

fn parse_stat(text: &str) -> Meta {
    let mut parts = text.trim().split('|');
    let ty = parts.next().unwrap_or("").to_ascii_lowercase();
    let kind = if ty.contains("link") {
        Kind::Link
    } else if ty.contains("directory") {
        Kind::Dir
    } else if ty.contains("regular") || ty.contains("file") {
        Kind::File
    } else if ty.is_empty() {
        Kind::Missing
    } else {
        Kind::Other
    };
    let mode = parts
        .next()
        .and_then(|m| u32::from_str_radix(m.trim(), 8).ok())
        .unwrap_or(0);
    Meta {
        kind,
        mode,
        owner: parts.next().unwrap_or("").to_string(),
        group: parts.next().unwrap_or("").to_string(),
    }
}

#[cfg(unix)]
fn mode_of(m: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    m.permissions().mode() & 0o7777
}

#[cfg(not(unix))]
fn mode_of(_m: &std::fs::Metadata) -> u32 {
    0
}

fn kind_of(m: &std::fs::Metadata) -> Kind {
    let t = m.file_type();
    if t.is_symlink() {
        Kind::Link
    } else if t.is_dir() {
        Kind::Dir
    } else if t.is_file() {
        Kind::File
    } else {
        Kind::Other
    }
}

// ── probe and apply ───────────────────────────────────────────────────────

impl Spec {
    /// Probe only. Touches nothing.
    pub(crate) fn plan(&self, ctx: &Ctx<'_>) -> Effect {
        self.reach(ctx, false)
    }

    pub(crate) fn apply(&self, ctx: &Ctx<'_>) -> Effect {
        self.reach(ctx, true)
    }

    /// One body for both, because a plan that does not compute exactly what
    /// apply would do is a plan that can disagree with it.
    fn reach(&self, ctx: &Ctx<'_>, act: bool) -> Effect {
        if ctx.sudo && !ctx.root_available {
            return Effect::Unprobed;
        }
        let meta = match stat(self, ctx) {
            Ok(m) => m,
            Err(msg) => return failed(msg),
        };
        match self.state {
            State::Absent => self.absent(ctx, &meta, act),
            State::Dir => self.dir(ctx, &meta, act),
            State::Link => self.link(ctx, &meta, act),
            State::File => self.file(ctx, &meta, act),
        }
    }

    // Spec §6.3: a path that is already gone is `ok`, not `changed`. That is
    // the case the second run of every idempotency test lands on.
    fn absent(&self, ctx: &Ctx<'_>, meta: &Meta, act: bool) -> Effect {
        if meta.kind == Kind::Missing {
            return Effect::Ok;
        }
        let non_empty_dir = meta.kind == Kind::Dir && !self.dir_is_empty(ctx);
        if non_empty_dir && !self.force {
            return failed(format!(
                "{} is a directory and is not empty; set `force: true` to remove it",
                self.path
            ));
        }
        if act && let Err(e) = self.remove(ctx, meta.kind) {
            return failed(e);
        }
        Effect::Changed(Some(format!("remove {}", self.path)))
    }

    fn dir(&self, ctx: &Ctx<'_>, meta: &Meta, act: bool) -> Effect {
        match meta.kind {
            Kind::Missing => {
                let mode = self.mode.unwrap_or(0o755);
                if act && let Err(e) = self.make_dir(ctx, mode) {
                    return failed(e);
                }
                Effect::Changed(Some(format!(
                    "create directory {} mode {mode:04o}",
                    self.path
                )))
            }
            Kind::Dir => self.metadata_only(ctx, meta, act),
            _ => failed(format!("{} exists and is not a directory", self.path)),
        }
    }

    fn link(&self, ctx: &Ctx<'_>, meta: &Meta, act: bool) -> Effect {
        let want = self.link_to.as_deref().unwrap_or_default();
        match meta.kind {
            Kind::Link => {
                let have = self.read_link(ctx);
                if have.as_deref() == Some(want) {
                    return Effect::Ok;
                }
                if act && let Err(e) = self.make_link(ctx, want) {
                    return failed(e);
                }
                Effect::Changed(Some(format!(
                    "link {} → {want} (was {})",
                    self.path,
                    have.unwrap_or_else(|| "?".into())
                )))
            }
            Kind::Missing => {
                if act && let Err(e) = self.make_link(ctx, want) {
                    return failed(e);
                }
                Effect::Changed(Some(format!("link {} → {want}", self.path)))
            }
            // Spec §6.3: replacing a real file or directory with a link is
            // destructive enough to have to be asked for.
            _ => {
                if !self.force {
                    return failed(format!(
                        "{} exists and is not a symlink; set `force: true` to replace it",
                        self.path
                    ));
                }
                if act {
                    if let Err(e) = self.remove(ctx, meta.kind) {
                        return failed(e);
                    }
                    if let Err(e) = self.make_link(ctx, want) {
                        return failed(e);
                    }
                }
                Effect::Changed(Some(format!("replace {} with a link to {want}", self.path)))
            }
        }
    }

    fn file(&self, ctx: &Ctx<'_>, meta: &Meta, act: bool) -> Effect {
        let want = self.content.as_deref().unwrap_or_default();
        match meta.kind {
            Kind::Missing => {
                let mode = self.mode.unwrap_or(0o644);
                if act && let Err(e) = self.write(ctx, want, mode) {
                    return failed(e);
                }
                Effect::Changed(Some(diff(
                    "",
                    &String::from_utf8_lossy(want),
                    self.label.as_deref().unwrap_or(&self.path),
                )))
            }
            Kind::File => {
                let have = match self.read(ctx) {
                    Ok(b) => b,
                    Err(e) => return failed(e),
                };
                if have == want {
                    return self.metadata_only(ctx, meta, act);
                }
                // An existing target keeps the mode it has unless `mode` says
                // otherwise (spec §6.3).
                let mode = self.mode.unwrap_or(meta.mode);
                if act {
                    if let Err(e) = self.write(ctx, want, mode) {
                        return failed(e);
                    }
                    if let Err(e) = self.own(ctx) {
                        return failed(e);
                    }
                }
                Effect::Changed(Some(diff(
                    &String::from_utf8_lossy(&have),
                    &String::from_utf8_lossy(want),
                    self.label.as_deref().unwrap_or(&self.path),
                )))
            }
            _ => failed(format!("{} exists and is not a regular file", self.path)),
        }
    }

    /// The content already matches, or there is none: only mode, owner and
    /// group are left to compare.
    fn metadata_only(&self, ctx: &Ctx<'_>, meta: &Meta, act: bool) -> Effect {
        let mut deltas = Vec::new();
        if let Some(m) = self.mode.filter(|m| *m != meta.mode) {
            deltas.push(format!("mode {:04o} → {m:04o}", meta.mode));
        }
        if let Some(o) = self.owner.as_deref().filter(|o| *o != meta.owner) {
            deltas.push(format!("owner {} → {o}", blank(&meta.owner)));
        }
        if let Some(g) = self.group.as_deref().filter(|g| *g != meta.group) {
            deltas.push(format!("group {} → {g}", blank(&meta.group)));
        }
        if deltas.is_empty() {
            return Effect::Ok;
        }
        if act {
            if let Some(m) = self.mode
                && let Err(e) = self.chmod(ctx, m)
            {
                return failed(e);
            }
            if let Err(e) = self.own(ctx) {
                return failed(e);
            }
        }
        Effect::Changed(Some(deltas.join("\n")))
    }

    // ── the operations, once each, sudo and not ──────────────────────────

    fn read(&self, ctx: &Ctx<'_>) -> std::result::Result<Vec<u8>, String> {
        if ctx.reads_as_root() {
            let got = ctx
                .as_root(&["cat", &self.path])
                .map_err(|e| format!("cannot run cat: {e}"))?;
            if got.how != crate::exec::process::How::Exited {
                return Err(format!("reading {} did not finish", self.path));
            }
            if got.rc != 0 {
                return Err(format!(
                    "cannot read {} as root: {}",
                    self.path,
                    got.stderr_text().trim()
                ));
            }
            return Ok(got.stdout);
        }
        std::fs::read(&self.path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::PermissionDenied {
                format!(
                    "cannot read {} ({e}); add `sudo: true` to read it as root",
                    self.path
                )
            } else {
                format!("cannot read {}: {e}", self.path)
            }
        })
    }

    fn read_link(&self, ctx: &Ctx<'_>) -> Option<String> {
        if ctx.reads_as_root() {
            let got = ctx.as_root(&["readlink", &self.path]).ok()?;
            if got.rc != 0 {
                return None;
            }
            return Some(String::from_utf8_lossy(&got.stdout).trim_end().to_string());
        }
        std::fs::read_link(&self.path)
            .ok()
            .map(|p| p.display().to_string())
    }

    fn dir_is_empty(&self, ctx: &Ctx<'_>) -> bool {
        if ctx.reads_as_root() {
            return match ctx.as_root(&["ls", "-A", &self.path]) {
                Ok(got) => got.stdout.iter().all(u8::is_ascii_whitespace),
                Err(_) => false,
            };
        }
        std::fs::read_dir(&self.path).is_ok_and(|mut d| d.next().is_none())
    }

    fn write(&self, ctx: &Ctx<'_>, bytes: &[u8], mode: u32) -> std::result::Result<(), String> {
        use std::io::Write;
        let e = |what: &str, err: std::io::Error| format!("cannot {what}: {err}");

        if ctx.sudo {
            // `install -D` would do this, but BSD install has no -D. Two
            // portable calls beat one that works on only half the fleet.
            if self.make_parents
                && let Some(parent) = Path::new(&self.path).parent()
                && !parent.as_os_str().is_empty()
            {
                let dir = parent.display().to_string();
                if let Some(bad) = ctx.perform(&["install", "-d", "-m", "0755", &dir], true) {
                    return Err(describe(bad));
                }
            }
            // Not next to the destination: being unable to create a file
            // there is usually the reason the step said sudo. `install`
            // copies, so nothing here depends on a shared filesystem.
            let mut tmp = tempfile::NamedTempFile::new().map_err(|x| e("make a temp file", x))?;
            tmp.write_all(bytes)
                .map_err(|x| e("write the temp file", x))?;
            tmp.flush().map_err(|x| e("write the temp file", x))?;
            set_mode(tmp.path(), 0o600).map_err(|x| e("secure the temp file", x))?;
            let staged = tmp.into_temp_path();

            let mode_arg = format!("{mode:04o}");
            let staged_str = staged.display().to_string();
            let mut argv = vec!["install", "-m", &mode_arg];
            if let Some(o) = &self.owner {
                argv.extend(["-o", o]);
            }
            if let Some(g) = &self.group {
                argv.extend(["-g", g]);
            }
            argv.extend([staged_str.as_str(), self.path.as_str()]);
            if let Some(bad) = ctx.perform(&argv, true) {
                return Err(describe(bad));
            }
            return Ok(());
        }

        // No sudo: the temp file goes next to the destination, because rename
        // is only atomic within one filesystem.
        let parent = Path::new(&self.path).parent();
        let dir = match parent {
            Some(p) if !p.as_os_str().is_empty() => p,
            // A bare relative path has `Some("")` for a parent, which is the
            // current directory said the long way.
            _ => Path::new("."),
        };
        if self.make_parents && !dir.exists() {
            create_dirs(dir, 0o755).map_err(|x| e(&format!("create {}", dir.display()), x))?;
        }
        let mut tmp = tempfile::NamedTempFile::new_in(dir)
            .map_err(|x| e(&format!("make a temp file in {}", dir.display()), x))?;
        tmp.write_all(bytes)
            .map_err(|x| e("write the temp file", x))?;
        tmp.flush().map_err(|x| e("write the temp file", x))?;
        let staged = tmp.into_temp_path();
        set_mode(&staged, mode).map_err(|x| e("set the mode", x))?;
        staged
            .persist(&self.path)
            .map_err(|x| format!("cannot place {}: {x}", self.path))?;
        Ok(())
    }

    fn make_dir(&self, ctx: &Ctx<'_>, mode: u32) -> std::result::Result<(), String> {
        if ctx.sudo {
            let m = format!("{mode:04o}");
            let mut argv = vec!["install", "-d", "-m", &m];
            if let Some(o) = &self.owner {
                argv.extend(["-o", o]);
            }
            if let Some(g) = &self.group {
                argv.extend(["-g", g]);
            }
            argv.push(&self.path);
            if let Some(bad) = ctx.perform(&argv, true) {
                return Err(describe(bad));
            }
            return Ok(());
        }
        create_dirs(Path::new(&self.path), mode)
            .map_err(|e| format!("cannot create {}: {e}", self.path))
    }

    fn make_link(&self, ctx: &Ctx<'_>, target: &str) -> std::result::Result<(), String> {
        if ctx.sudo {
            if let Some(bad) = ctx.perform(&["ln", "-sfn", target, &self.path], true) {
                return Err(describe(bad));
            }
            return Ok(());
        }
        // -sfn without a shell: remove whatever is there, then link.
        #[expect(
            clippy::let_underscore_must_use,
            reason = "removing what may not exist; the symlink below is the operation"
        )]
        let _ = std::fs::remove_file(&self.path);
        symlink(target, &self.path).map_err(|e| format!("cannot link {}: {e}", self.path))
    }

    fn remove(&self, ctx: &Ctx<'_>, kind: Kind) -> std::result::Result<(), String> {
        if ctx.sudo {
            let flag = if self.force { "-rf" } else { "-r" };
            if let Some(bad) = ctx.perform(&["rm", flag, &self.path], true) {
                return Err(describe(bad));
            }
            return Ok(());
        }
        let r = match kind {
            Kind::Dir if self.force => std::fs::remove_dir_all(&self.path),
            Kind::Dir => std::fs::remove_dir(&self.path),
            _ => std::fs::remove_file(&self.path),
        };
        r.map_err(|e| format!("cannot remove {}: {e}", self.path))
    }

    fn chmod(&self, ctx: &Ctx<'_>, mode: u32) -> std::result::Result<(), String> {
        if ctx.sudo {
            let m = format!("{mode:04o}");
            if let Some(bad) = ctx.perform(&["chmod", &m, &self.path], true) {
                return Err(describe(bad));
            }
            return Ok(());
        }
        set_mode(Path::new(&self.path), mode)
            .map_err(|e| format!("cannot chmod {}: {e}", self.path))
    }

    /// `owner` and `group` are independent and both require sudo, so there is
    /// no non-sudo path here to write.
    fn own(&self, ctx: &Ctx<'_>) -> std::result::Result<(), String> {
        if self.owner.is_none() && self.group.is_none() {
            return Ok(());
        }
        let spec = format!(
            "{}:{}",
            self.owner.as_deref().unwrap_or(""),
            self.group.as_deref().unwrap_or("")
        );
        if let Some(bad) = ctx.perform(&["chown", &spec, &self.path], true) {
            return Err(describe(bad));
        }
        Ok(())
    }
}

// ── helpers ───────────────────────────────────────────────────────────────

fn failed(msg: impl Into<String>) -> Effect {
    Effect::fail(msg)
}

fn blank(s: &str) -> &str {
    if s.is_empty() { "?" } else { s }
}

/// `perform` already worded the failure; these helpers return a String, so
/// unwrap it back out rather than inventing a second phrasing.
fn describe(e: Effect) -> String {
    match e {
        Effect::Failed { msg, .. } => msg,
        _ => "failed".to_string(),
    }
}

/// Unified, three lines of context (spec §9.1). Content that is not text gets
/// a sentence instead: a diff of mojibake helps nobody.
fn diff(old: &str, new: &str, path: &str) -> String {
    if old.contains('\0') || new.contains('\0') {
        return format!("{path}: binary content differs");
    }
    let d = similar::TextDiff::from_lines(old, new);
    let text = d
        .unified_diff()
        .context_radius(3)
        .header("current", path)
        .to_string();
    if text.trim().is_empty() {
        format!("{path}: content differs")
    } else {
        text
    }
}

/// Create the target and any missing parents, giving the parents `0755` and
/// the leaf `mode`.
///
/// `create_dir_all` would give every level a umask-derived mode, which is
/// what §6.3 says modes are not. GNU `install -d -m` already behaves this
/// way — intermediates are 0755 under any umask, the mode lands on the leaf —
/// so this is the non-sudo path matching the sudo one rather than a third
/// rule. Only directories this creates are touched; an existing parent keeps
/// whatever it has.
fn create_dirs(path: &Path, leaf_mode: u32) -> std::io::Result<()> {
    let mut missing = Vec::new();
    let mut cur = Some(path);
    // `Path::new("foo").parent()` is `Some("")`, not `None`, and `""` never
    // exists — so an unguarded walk ends up asking for `create_dir("")`,
    // which is ENOENT. A bare relative path is odd in a plan but not
    // forbidden, and it should not crash.
    while let Some(p) = cur.filter(|p| !p.as_os_str().is_empty() && !p.exists()) {
        missing.push(p);
        cur = p.parent();
    }
    for (i, dir) in missing.iter().rev().enumerate() {
        std::fs::create_dir(dir)?;
        let mode = if i + 1 == missing.len() {
            leaf_mode
        } else {
            0o755
        };
        set_mode(dir, mode)?;
    }
    Ok(())
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> std::io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn symlink(target: &str, path: &str) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, path)
}

#[cfg(not(unix))]
fn symlink(_target: &str, _path: &str) -> std::io::Result<()> {
    Err(std::io::Error::other(
        "symlinks are not supported on this platform",
    ))
}
