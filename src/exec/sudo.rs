//! Privilege escalation. Spec §7, D8: root or the current user, nothing else.
//!
//! The whole design is one preflight and one `wrap`. If sudo is going to ask
//! for a password, the run has to find that out before the first step rather
//! than three minutes in, behind a spinner, with the terminal in a state
//! nobody can type into.

use crate::error::Diag;
use std::io::Write;

pub(crate) struct Sudo {
    /// `Some` only with `--ask-sudo-pass`. Fed to `sudo -S` for every
    /// escalated step, because sudo's own credential cache is not something
    /// to rely on across a long run.
    password: Option<String>,
}

impl Sudo {
    /// `needed` comes from the walk `apply` does before executing anything.
    pub(crate) fn preflight(needed: bool, ask: bool) -> Result<Sudo, Diag> {
        if !needed {
            return Ok(Sudo { password: None });
        }
        if cfg!(windows) {
            return Err(
                Diag::file_level("sudo", "`sudo` is not supported on Windows")
                    .with_note("run provision from an elevated PowerShell prompt instead"),
            );
        }
        if ask {
            let password = read_password()?;
            // Prove it before the first step, exactly as the -n path does.
            let ok = std::process::Command::new("sudo")
                .args(["-S", "-p", "", "--", "true"])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .and_then(|mut c| {
                    if let Some(mut p) = c.stdin.take() {
                        #[expect(
                            clippy::let_underscore_must_use,
                            reason = "sudo may have a cached credential and never read stdin"
                        )]
                        let _ = p.write_all(format!("{password}\n").as_bytes());
                    }
                    c.wait()
                })
                .is_ok_and(|st| st.success());
            if !ok {
                return Err(Diag::file_level("sudo", "that password was not accepted"));
            }
            return Ok(Sudo {
                password: Some(password),
            });
        }

        if root_is_reachable() {
            Ok(Sudo { password: None })
        } else {
            Err(Diag::file_level(
                "sudo",
                "this plan has steps with `sudo: true` and sudo wants a password",
            )
            .with_note("re-run with --ask-sudo-pass, or warm the credential with `sudo -v` first"))
        }
    }

    /// No escalation, for a run that needs none.
    pub(crate) fn none() -> Sudo {
        Sudo { password: None }
    }

    /// Wrap an argv so it runs as root. `env_keys` are the step's own `env`
    /// keys and nothing else — sudo drops the environment by default, and
    /// widening that is how a step picks up a variable it never declared.
    pub(crate) fn wrap(&self, argv: Vec<String>, env_keys: &[String]) -> Vec<String> {
        let mut out = vec!["sudo".to_string()];
        match &self.password {
            // `-k` first, so sudo always reads the password line this run
            // feeds it. Without it a still-valid timestamp makes sudo skip the
            // read, and the password stays in the pipe for the child to find
            // on its own stdin (spec §10).
            Some(_) => out.extend([
                "-k".to_string(),
                "-S".to_string(),
                "-p".to_string(),
                String::new(),
            ]),
            None => out.push("-n".to_string()),
        }
        if !env_keys.is_empty() {
            out.push(format!("--preserve-env={}", env_keys.join(",")));
        }
        out.push("--".to_string());
        out.extend(argv);
        out
    }

    /// What to feed the wrapped command's stdin, if anything.
    pub(crate) fn stdin(&self) -> Option<String> {
        self.password.as_ref().map(|p| format!("{p}\n"))
    }
}

/// Read a password from the terminal with echo off. `rpassword` in twenty
/// lines; the crate would be the fourth dependency added for this phase.
#[cfg(unix)]
#[expect(
    unsafe_code,
    reason = "reading a password without echo needs termios; std has no equivalent"
)]
fn read_password() -> Result<String, Diag> {
    use std::io::BufRead;

    let fail = |m: &str| Diag::file_level("sudo", m.to_string());
    eprint!("  [sudo] password: ");
    // The prompt is on stderr; a failed flush of stderr has nowhere to go.
    #[expect(clippy::unused_result_ok, reason = "the prompt is best-effort")]
    std::io::stderr().flush().ok();

    // SAFETY: `termios` is a plain C struct of integers and arrays, so an
    // all-zero value is a valid one; `tcgetattr` overwrites it wholesale
    // before anything reads it, and its return code says whether it did.
    let mut term: libc::termios = unsafe { std::mem::zeroed() };
    // SAFETY: `tcgetattr` writes through the pointer to a `termios` we own
    // and outlives the call. A non-zero return means stdin is not a tty, and
    // `have_tty` is what everything below keys on.
    let have_tty = unsafe { libc::tcgetattr(libc::STDIN_FILENO, &mut term) } == 0;
    let restore = term;
    if have_tty {
        term.c_lflag &= !libc::ECHO;
        // SAFETY: reached only when `tcgetattr` succeeded, so stdin is a tty
        // and `term` is the settings it just handed us with ECHO cleared.
        unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &term) };
    }

    let mut line = String::new();
    let read = std::io::stdin().lock().read_line(&mut line);

    if have_tty {
        // SAFETY: `restore` is the copy taken before ECHO was cleared, so
        // this puts the terminal back exactly as it was found. Unconditional
        // on how the read went: a failed read must not leave echo off.
        unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &restore) };
    }
    eprintln!();

    read.map_err(|e| fail(&format!("could not read the password: {e}")))?;
    let line = line.trim_end_matches(['\r', '\n']).to_string();
    if line.is_empty() {
        return Err(fail("no password given"));
    }
    Ok(line)
}

#[cfg(not(unix))]
fn read_password() -> Result<String, Diag> {
    Err(Diag::file_level(
        "sudo",
        "`sudo` is not supported on Windows",
    ))
}

/// Can sudo escalate right now without asking for anything? `apply` turns a
/// `false` here into a hard stop before the first step; `plan` only records it.
pub(crate) fn root_is_reachable() -> bool {
    std::process::Command::new("sudo")
        .args(["-n", "--", "true"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|st| st.success())
}
