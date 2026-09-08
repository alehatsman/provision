//! `download` — a file fetched from a URL, and proof it is the right file.
//! Spec §6.9.
//!
//! The fleet had twelve `curl -o` steps, every one gated by `creates` and
//! none by content: the shell reads "the file exists" well and "the file is
//! the right one" never. What this action adds is the hash and the verdict,
//! not the transport — curl still does the fetching, because every fleet OS
//! ships it and a static binary carrying its own TLS stack is the wrong trade
//! for something the shell already does well.
//!
//! Placement mirrors `file` (§6.3) rather than delegating to it: `file`
//! reports a change as a text diff of the content, which on a tarball is
//! noise, and it takes the bytes in memory. The two branches are the same
//! two — a temp file beside the destination without sudo, because rename is
//! only atomic within one filesystem, and a staged file installed with sudo,
//! because being unable to write next to the destination is usually why the
//! step said sudo.

use super::{Ctx, Effect};
use crate::config::model::Step;
use crate::error::Result;
use crate::template::{Engine, expanduser};
use minijinja::Value;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Spec {
    pub url: String,
    pub dest: String,
    /// Lower-case hex. `None` means the plan does not say the content
    /// matters, and §6.9 takes that literally: an existing `dest` is `ok`.
    pub sha256: Option<String>,
    pub mode: Option<u32>,
    pub owner: Option<String>,
    pub group: Option<String>,
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

    let Some(url_at) = body.get("url") else {
        return Err(body.err("`download` requires `url`"));
    };
    let Some(url) = render(url_at.as_str()?) else {
        return Ok(None);
    };
    let Some(dest_at) = body.get("dest") else {
        return Err(body
            .err("`download` requires `dest`")
            .with_note("the path the file ends up at, not a directory"));
    };
    let Some(dest) = render(dest_at.as_str()?) else {
        return Ok(None);
    };

    let sha256 = match body.get("sha256") {
        Some(n) => {
            let Some(text) = render(&n.as_scalar_string()?) else {
                return Ok(None);
            };
            let text = text.trim().to_ascii_lowercase();
            // A truncated or mistyped digest would fail every fetch with a
            // mismatch, which reads as a bad download rather than a bad plan.
            if text.len() != 64 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(n
                    .err("`sha256` is not a sha256 digest")
                    .with_note("64 hex characters"));
            }
            Some(text)
        }
        None => None,
    };

    let Some(meta) = super::file::parse_metadata(step, engine, ctx, raw)? else {
        return Ok(None);
    };

    Ok(Some(Spec {
        url: url.trim().to_string(),
        dest: expanduser(dest.trim()),
        sha256,
        mode: meta.mode,
        owner: meta.owner,
        group: meta.group,
    }))
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
        let there = self.exists(ctx);

        // Spec §6.9: without `sha256` a present `dest` is `ok` and is never
        // fetched again. A URL is not state, and re-downloading on every
        // apply to find out what changed is exactly what the twelve
        // `creates`-gated curls were avoiding.
        let Some(want) = self.sha256.as_deref() else {
            if there {
                return Effect::Ok;
            }
            return self.fetch_into_place(ctx, act, "no sha256 declared");
        };

        if there {
            match self.digest(ctx) {
                Ok(have) if have == want => return Effect::Ok,
                Ok(have) => {
                    return self.fetch_into_place(
                        ctx,
                        act,
                        &format!("sha256 {} → {}", short(&have), short(want)),
                    );
                }
                Err(msg) => return Effect::fail(msg),
            }
        }
        self.fetch_into_place(ctx, act, &format!("sha256 {}", short(want)))
    }

    /// Plan never fetches (§6.9), so this is where both verbs part company.
    fn fetch_into_place(&self, ctx: &Ctx<'_>, act: bool, why: &str) -> Effect {
        let detail = format!("{} → {} ({why})", self.url, self.dest);
        if !act {
            return Effect::Changed(Some(detail));
        }
        // `curl` is executed from PATH, and its absence is said plainly
        // rather than arriving as a spawn error nobody can act on.
        if ctx.exec(&["curl", "--version"], false).is_err() {
            return Effect::fail("`curl` is not on PATH");
        }
        let staged = match self.staging_dir(ctx) {
            Ok(dir) => match tempfile::NamedTempFile::new_in(&dir) {
                Ok(f) => f,
                Err(e) => {
                    return Effect::fail(format!(
                        "cannot make a temp file in {}: {e}",
                        dir.display()
                    ));
                }
            },
            Err(bad) => return bad,
        };
        let tmp = staged.path().display().to_string();

        // `--retry 0`: the step's own `retry` (§4) repeats the whole
        // fetch-and-hash, and two retry mechanisms stacked would multiply.
        // `-f` makes a non-2xx an error instead of a saved error page, `-L`
        // follows redirects, `-sS` is quiet but keeps the reason on stderr.
        if let Some(bad) = ctx.perform(
            &["curl", "-fsSL", "--retry", "0", "-o", &tmp, &self.url],
            false,
        ) {
            return bad;
        }

        if let Some(want) = self.sha256.as_deref() {
            let have = match hash_file(staged.path()) {
                Ok(h) => h,
                Err(e) => return Effect::fail(e),
            };
            // Spec §6.9: name both, remove the temp file, leave `dest` as it
            // was. The temp file goes when `staged` drops, on every path out.
            if have != want {
                return Effect::Failed {
                    msg: format!("{} is not the declared file", self.url),
                    detail: format!("declared sha256 {want}\nreceived sha256 {have}"),
                    interrupted: false,
                };
            }
        }

        if let Some(bad) = self.place(ctx, staged) {
            return bad;
        }
        Effect::Changed(Some(detail))
    }

    // ── the operations ───────────────────────────────────────────────────

    /// Where the temp file goes. Beside `dest` without sudo, so the rename is
    /// atomic; in the user's own temp with it, because the destination's
    /// directory is usually the one this step cannot write.
    fn staging_dir(&self, ctx: &Ctx<'_>) -> std::result::Result<PathBuf, Effect> {
        if ctx.sudo {
            if let Some(bad) = self.make_parents(ctx) {
                return Err(bad);
            }
            return Ok(std::env::temp_dir());
        }
        let dir = parent_of(&self.dest);
        if !dir.exists()
            && let Err(e) = std::fs::create_dir_all(&dir)
        {
            return Err(Effect::fail(format!(
                "cannot create {}: {e}",
                dir.display()
            )));
        }
        Ok(dir)
    }

    fn make_parents(&self, ctx: &Ctx<'_>) -> Option<Effect> {
        let dir = parent_of(&self.dest).display().to_string();
        ctx.perform(&["install", "-d", "-m", "0755", &dir], true)
    }

    /// Move the verified file into place. `install` under sudo, because it
    /// copies and so does not need a shared filesystem; a rename otherwise,
    /// which is atomic because the temp file is in the same directory.
    fn place(&self, ctx: &Ctx<'_>, staged: tempfile::NamedTempFile) -> Option<Effect> {
        let mode = self.mode.unwrap_or(0o644);
        if ctx.sudo {
            if let Err(e) = set_mode(staged.path(), 0o600) {
                return Some(Effect::fail(format!("cannot secure the temp file: {e}")));
            }
            let kept = staged.into_temp_path();
            let from = kept.display().to_string();
            let mode_arg = format!("{mode:04o}");
            let mut argv = vec!["install", "-m", &mode_arg];
            if let Some(o) = &self.owner {
                argv.extend(["-o", o]);
            }
            if let Some(g) = &self.group {
                argv.extend(["-g", g]);
            }
            argv.extend([from.as_str(), self.dest.as_str()]);
            return ctx.perform(&argv, true);
        }
        if let Err(e) = set_mode(staged.path(), mode) {
            return Some(Effect::fail(format!("cannot set the mode: {e}")));
        }
        staged
            .persist(&self.dest)
            .err()
            .map(|e| Effect::fail(format!("cannot move the file to {}: {e}", self.dest)))
    }

    fn exists(&self, ctx: &Ctx<'_>) -> bool {
        if ctx.reads_as_root() {
            return ctx
                .as_root(&["test", "-f", &self.dest])
                .is_ok_and(|g| g.rc == 0);
        }
        Path::new(&self.dest).is_file()
    }

    /// The hash of what is already at `dest`. Reading obeys `sudo` the way
    /// `file`'s does (§6.3): a target the step needs root to write is usually
    /// one it needs root to read.
    fn digest(&self, ctx: &Ctx<'_>) -> std::result::Result<String, String> {
        if ctx.reads_as_root() {
            let got = ctx
                .as_root(&["cat", &self.dest])
                .map_err(|e| format!("cannot run cat: {e}"))?;
            if got.rc != 0 {
                return Err(format!(
                    "cannot read {} as root: {}",
                    self.dest,
                    got.stderr_text().trim()
                ));
            }
            return Ok(hex(&Sha256::digest(&got.stdout)));
        }
        hash_file(Path::new(&self.dest))
    }
}

/// Streamed, so a large artifact is not held in memory to be hashed.
fn hash_file(path: &Path) -> std::result::Result<String, String> {
    let mut file =
        std::fs::File::open(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        // Writing to a String cannot fail; the Result is `fmt`'s signature.
        #[expect(
            clippy::let_underscore_must_use,
            reason = "writing to a String is infallible"
        )]
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Enough of a digest to tell two apart in one line of output.
fn short(digest: &str) -> &str {
    digest.get(..12).unwrap_or(digest)
}

/// A bare relative path has `Some("")` for a parent, which is the current
/// directory said the long way.
fn parent_of(path: &str) -> PathBuf {
    match Path::new(path).parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hex_is_lower_case_and_padded() {
        assert_eq!(hex(&[0x00, 0x0f, 0xff]), "000fff");
    }

    /// The empty-string vector, so a wrong digest encoding is caught here
    /// rather than as a mismatch on a real download.
    #[test]
    fn the_digest_matches_the_published_vector() {
        assert_eq!(
            hex(&Sha256::digest(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hex(&Sha256::digest(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn a_bare_filename_stages_in_the_current_directory() {
        assert_eq!(parent_of("file.gz"), PathBuf::from("."));
        assert_eq!(parent_of("/a/b/file.gz"), PathBuf::from("/a/b"));
    }
}
