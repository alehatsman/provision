//! `git` — ensure a checkout of a repository at a ref. Spec §6.8.
//!
//! One idea runs through the whole module: a ref's **kind** decides
//! everything, and the name alone never tells you the kind. `v0.3.0` may be a
//! tag today and a branch tomorrow, and an annotated tag's ref does not
//! resolve to the commit HEAD sits on — it resolves to the tag object. So the
//! kind is asked of the repository (`refs/tags/…`, then `refs/remotes/origin/…`)
//! and every comparison goes through `^{commit}`. That peel is not a detail:
//! without it an annotated tag compares unequal forever, which looks exactly
//! like working, just always converging.

use super::{Ctx, Effect};
use crate::config::model::Step;
use crate::error::Result;
use crate::template::{Engine, expanduser};
use minijinja::Value;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Spec {
    pub repo: String,
    pub dest: String,
    /// `None` is the remote's default branch, and is treated as a branch.
    pub reference: Option<String>,
}

/// What a ref turned out to be, once the repository was asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// A tag or a sha: immutable, so the answer needs no network once it is
    /// on disk.
    Pinned,
    /// A branch: mutable, so only a fetch can say whether it moved.
    Branch,
    /// Names nothing this checkout has heard of. Only a fetch can resolve it.
    Unresolved,
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

    let Some(repo_at) = body.get("repo") else {
        return Err(body
            .err("`git` requires `repo`")
            .with_note("the URL to clone from"));
    };
    let Some(repo) = render(repo_at.as_str()?) else {
        return Ok(None);
    };

    let Some(dest_at) = body.get("dest") else {
        return Err(body
            .err("`git` requires `dest`")
            .with_note("the directory the checkout lives in"));
    };
    let Some(dest) = render(dest_at.as_str()?) else {
        return Ok(None);
    };

    let reference = match body.get("ref") {
        Some(n) => {
            let Some(text) = render(n.as_str()?) else {
                return Ok(None);
            };
            let text = text.trim().to_string();
            if text.is_empty() {
                return Err(n
                    .err("`ref` is empty")
                    .with_note("omit it for the remote's default branch"));
            }
            Some(text)
        }
        None => None,
    };

    Ok(Some(Spec {
        repo: repo.trim().to_string(),
        dest: expanduser(dest.trim()),
        reference,
    }))
}

// ── asking the repository ─────────────────────────────────────────────────

impl Spec {
    /// `git -C <dest> …`, captured. `None` means git could not be run at all.
    fn git(&self, ctx: &Ctx<'_>, args: &[&str]) -> Option<crate::exec::process::Raw> {
        let mut argv = vec!["git", "-C", self.dest.as_str()];
        argv.extend_from_slice(args);
        ctx.exec(&argv, ctx.sudo).ok()
    }

    /// The first line of stdout, when the command succeeded.
    fn ask(&self, ctx: &Ctx<'_>, args: &[&str]) -> Option<String> {
        let got = self.git(ctx, args)?;
        if got.rc != 0 {
            return None;
        }
        let text = String::from_utf8_lossy(&got.stdout);
        let line = text.lines().next()?.trim();
        (!line.is_empty()).then(|| line.to_string())
    }

    fn succeeds(&self, ctx: &Ctx<'_>, args: &[&str]) -> bool {
        self.git(ctx, args).is_some_and(|g| g.rc == 0)
    }

    /// Ask the repository what this ref is. Tags first: a name that is both a
    /// tag and a branch is a tag here, and §6.8 says the tag is what
    /// `rev-parse --verify refs/tags/<ref>` finds.
    fn kind(&self, ctx: &Ctx<'_>) -> Kind {
        let Some(want) = self.reference.as_deref() else {
            // No ref is the remote's default branch, which is a branch.
            return Kind::Branch;
        };
        if self.succeeds(ctx, &["rev-parse", "--verify", "--quiet", &tag_ref(want)]) {
            return Kind::Pinned;
        }
        if self.succeeds(
            ctx,
            &["rev-parse", "--verify", "--quiet", &remote_ref(want)],
        ) {
            return Kind::Branch;
        }
        // Not a tag and not a remote branch, but resolvable: a sha, or a
        // local branch someone made. `--verify <x>^{commit}` answers both and
        // rejects a name that means nothing.
        if self.succeeds(ctx, &["rev-parse", "--verify", "--quiet", &peel(want)]) {
            // A local branch by that name is still a branch: it tracks
            // something that can move under it.
            if self.succeeds(ctx, &["rev-parse", "--verify", "--quiet", &head_ref(want)]) {
                return Kind::Branch;
            }
            return Kind::Pinned;
        }
        Kind::Unresolved
    }

    /// The branch this step is about: the named one, or the remote's default.
    ///
    /// `origin/HEAD` is what a clone records as the remote's default branch.
    /// A checkout made before that symref existed has none, and the branch
    /// checked out now is the only offline answer left — asking the remote
    /// would be the network call plan promised not to make.
    fn branch(&self, ctx: &Ctx<'_>) -> Option<String> {
        if let Some(name) = self.reference.clone() {
            return Some(name);
        }
        if let Some(full) = self.ask(ctx, &["rev-parse", "--abbrev-ref", "origin/HEAD"]) {
            return Some(full.trim_start_matches("origin/").to_string());
        }
        self.ask(ctx, &["rev-parse", "--abbrev-ref", "HEAD"])
    }
}

fn tag_ref(name: &str) -> String {
    format!("refs/tags/{name}")
}

fn head_ref(name: &str) -> String {
    format!("refs/heads/{name}")
}

fn remote_ref(name: &str) -> String {
    format!("refs/remotes/origin/{name}")
}

/// The `^{commit}` peel, §6.8's one non-negotiable detail.
fn peel(name: &str) -> String {
    format!("{name}^{{commit}}")
}

fn short(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

// ── the verdict ───────────────────────────────────────────────────────────

impl Spec {
    pub(crate) fn plan(&self, ctx: &Ctx<'_>) -> Effect {
        self.reach(ctx, false)
    }

    pub(crate) fn apply(&self, ctx: &Ctx<'_>) -> Effect {
        self.reach(ctx, true)
    }

    fn reach(&self, ctx: &Ctx<'_>, act: bool) -> Effect {
        if ctx.sudo && !ctx.root_available {
            return Effect::Unprobed;
        }
        // Spec §6.8: git comes from PATH, and its absence is said plainly
        // rather than surfacing as a failed `rev-parse` nobody asked for.
        if ctx.exec(&["git", "--version"], false).is_err() {
            return Effect::fail("`git` is not on PATH");
        }

        if !self.exists(ctx) {
            return self.clone_fresh(ctx, act);
        }
        if !self.succeeds(ctx, &["rev-parse", "--git-dir"]) {
            return Effect::fail(format!("{} exists and is not a git repository", self.dest));
        }
        // Spec §6.8: fails, never resets. Both checks come before anything
        // that could move HEAD.
        if let Some(bad) = self.guard(ctx) {
            return bad;
        }
        match self.kind(ctx) {
            Kind::Pinned => self.converge_pinned(ctx, act),
            Kind::Branch => self.converge_branch(ctx, act),
            // Nothing on disk answers this, and resolving it is a network
            // call. Plan does not make one (§6.5's rule for `latest`); apply
            // fetches and asks again.
            Kind::Unresolved => {
                if !act {
                    return Effect::Unknown;
                }
                if let Some(bad) = self.fetch(ctx) {
                    return bad;
                }
                match self.kind(ctx) {
                    Kind::Pinned => self.converge_pinned(ctx, act),
                    Kind::Branch => self.converge_branch(ctx, act),
                    Kind::Unresolved => Effect::fail(format!(
                        "{} names no tag, branch or commit in {}",
                        self.reference.as_deref().unwrap_or("HEAD"),
                        self.repo
                    )),
                }
            }
        }
    }

    fn exists(&self, ctx: &Ctx<'_>) -> bool {
        if ctx.sudo {
            return ctx
                .as_root(&["test", "-e", &self.dest])
                .is_ok_and(|g| g.rc == 0);
        }
        Path::new(&self.dest).exists()
    }

    /// A `dest` whose `origin` is a different repository is not this step's
    /// checkout, and converging it would mean rewriting someone else's.
    fn origin_url(&self, ctx: &Ctx<'_>) -> Option<String> {
        self.ask(ctx, &["remote", "get-url", "origin"])
    }

    /// The two conditions §6.8 fails on, asked before anything moves.
    fn guard(&self, ctx: &Ctx<'_>) -> Option<Effect> {
        match self.origin_url(ctx) {
            Some(url) if same_repo(&url, &self.repo) => {}
            Some(url) => {
                return Some(Effect::fail(format!(
                    "{} has origin {url}, not {}",
                    self.dest, self.repo
                )));
            }
            None => {
                return Some(Effect::fail(format!("{} has no `origin`", self.dest)));
            }
        }
        // Tracked modifications only. The guard exists so provision does not
        // throw away work it did not do, and a checkout cannot destroy an
        // untracked file — if one genuinely collides with the target ref, git
        // refuses the checkout itself and says so better than this could.
        let dirty = self
            .git(ctx, &["status", "--porcelain", "--untracked-files=no"])
            .is_some_and(|g| g.rc == 0 && !g.stdout.iter().all(u8::is_ascii_whitespace));
        if dirty {
            return Some(Effect::fail(format!(
                "{} has uncommitted changes; provision will not discard them",
                self.dest
            )));
        }
        None
    }

    fn head(&self, ctx: &Ctx<'_>) -> Option<String> {
        self.ask(ctx, &["rev-parse", "HEAD"])
    }

    // ── the three convergences ───────────────────────────────────────────

    fn clone_fresh(&self, ctx: &Ctx<'_>, act: bool) -> Effect {
        let at = self.reference.as_deref().unwrap_or("the default branch");
        if !act {
            return Effect::Changed(Some(format!("clone {} at {at}", self.repo)));
        }
        if let Some(bad) = self.make_parents(ctx) {
            return bad;
        }
        // Spec §6.8: full clones, no `depth` — a shallow clone breaks
        // `describe` and tag comparison.
        if let Some(bad) = ctx.perform(
            &["git", "clone", "--quiet", &self.repo, &self.dest],
            ctx.sudo,
        ) {
            return bad;
        }
        if self.reference.is_some()
            && let Some(bad) = self.checkout(ctx)
        {
            return bad;
        }
        Effect::Changed(Some(format!("clone {} at {at}", self.repo)))
    }

    /// A tag or a sha. Immutable, so the comparison is offline and exact.
    fn converge_pinned(&self, ctx: &Ctx<'_>, act: bool) -> Effect {
        let Some(want) = self
            .reference
            .as_deref()
            .and_then(|r| self.ask(ctx, &["rev-parse", "--verify", "--quiet", &peel(r)]))
        else {
            return Effect::fail(format!(
                "cannot resolve {} in {}",
                self.reference.as_deref().unwrap_or("HEAD"),
                self.dest
            ));
        };
        let Some(have) = self.head(ctx) else {
            return Effect::fail(format!("cannot read HEAD of {}", self.dest));
        };
        if have == want {
            return Effect::Ok;
        }
        if act && let Some(bad) = self.checkout(ctx) {
            return bad;
        }
        Effect::Changed(Some(format!("{} → {}", short(&have), short(&want))))
    }

    /// A branch. Whether the remote moved is not a question the local clone
    /// answers, so plan says so and apply goes and asks.
    fn converge_branch(&self, ctx: &Ctx<'_>, act: bool) -> Effect {
        if !act {
            return Effect::Unknown;
        }
        let Some(name) = self.branch(ctx) else {
            return Effect::fail(format!("cannot tell which branch {} is on", self.dest));
        };
        if let Some(bad) = self.fetch(ctx) {
            return bad;
        }
        let upstream = format!("origin/{name}");
        let Some(want) = self.ask(ctx, &["rev-parse", "--verify", "--quiet", &peel(&upstream)])
        else {
            return Effect::fail(format!("{} has no branch {name}", self.repo));
        };
        let Some(have) = self.head(ctx) else {
            return Effect::fail(format!("cannot read HEAD of {}", self.dest));
        };
        if have == want {
            return Effect::Ok;
        }
        // On the branch first, or `merge --ff-only` would move whatever HEAD
        // happens to be. A checkout of a branch that already exists locally
        // keeps its commits; it does not reset it.
        if self
            .ask(ctx, &["rev-parse", "--abbrev-ref", "HEAD"])
            .as_deref()
            != Some(name.as_str())
            && let Some(bad) = ctx.perform(
                &["git", "-C", &self.dest, "checkout", "--quiet", &name],
                ctx.sudo,
            )
        {
            return bad;
        }
        // `--ff-only`, so a branch that has diverged fails instead of being
        // rewritten. Spec §6.8: fails, never resets.
        if let Some(bad) = ctx.perform(
            &[
                "git",
                "-C",
                &self.dest,
                "merge",
                "--ff-only",
                "--quiet",
                &upstream,
            ],
            ctx.sudo,
        ) {
            return bad;
        }
        Effect::Changed(Some(format!("{} → {}", short(&have), short(&want))))
    }

    // ── the operations ───────────────────────────────────────────────────

    fn make_parents(&self, ctx: &Ctx<'_>) -> Option<Effect> {
        let parent = Path::new(&self.dest).parent()?;
        if parent.as_os_str().is_empty() {
            return None;
        }
        let dir = parent.display().to_string();
        if ctx.sudo {
            return ctx.perform(&["install", "-d", "-m", "0755", &dir], true);
        }
        std::fs::create_dir_all(parent)
            .err()
            .map(|e| Effect::fail(format!("cannot create {dir}: {e}")))
    }

    fn fetch(&self, ctx: &Ctx<'_>) -> Option<Effect> {
        ctx.perform(
            &[
                "git", "-C", &self.dest, "fetch", "--quiet", "--tags", "origin",
            ],
            ctx.sudo,
        )
    }

    /// Move HEAD onto the ref. A tag or sha lands detached, which §6.8 says
    /// is fine and is what a tag checkout leaves behind.
    fn checkout(&self, ctx: &Ctx<'_>) -> Option<Effect> {
        let name = self.reference.as_deref()?;
        ctx.perform(
            &["git", "-C", &self.dest, "checkout", "--quiet", name],
            ctx.sudo,
        )
    }
}

/// Two URLs naming the same repository. Only the differences git itself
/// treats as cosmetic are ignored: a trailing slash, and the `.git` suffix
/// that half the fleet writes and half does not.
fn same_repo(a: &str, b: &str) -> bool {
    normalize(a) == normalize(b)
}

fn normalize(url: &str) -> String {
    let u = url.trim().trim_end_matches('/');
    u.strip_suffix(".git").unwrap_or(u).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_trailing_slash_or_dot_git_is_the_same_repository() {
        assert!(same_repo(
            "https://github.com/a/b.git",
            "https://github.com/a/b"
        ));
        assert!(same_repo(
            "https://github.com/a/b/",
            "https://github.com/a/b"
        ));
        assert!(!same_repo(
            "https://github.com/a/b",
            "https://github.com/a/c"
        ));
    }

    #[test]
    fn the_peel_is_on_every_comparison() {
        assert_eq!(peel("v1.0"), "v1.0^{commit}");
        assert_eq!(tag_ref("v1.0"), "refs/tags/v1.0");
        assert_eq!(remote_ref("main"), "refs/remotes/origin/main");
    }
}
