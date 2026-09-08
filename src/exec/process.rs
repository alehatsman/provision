//! Spawning, capture, and the one kill path both `timeout` and Ctrl-C use.
//!
//! Every child is put in its own process group. That is not an optimisation:
//! it is what makes the two hard requirements of spec §10 possible. A group
//! means Ctrl-C at the terminal does *not* reach the child — only the
//! foreground group gets SIGINT — so provision decides when the child dies
//! and can print a summary afterwards. It also means `killpg` reaches the
//! grandchildren a bare `child.kill()` would leave running.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Set by the SIGINT handler. A handler may only touch async-signal-safe
/// things, and storing to an atomic is one of them; the killing happens on
/// the main thread, which is watching this flag.
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

pub(crate) fn interrupted() -> bool {
    INTERRUPTED.load(Ordering::SeqCst)
}

/// Install the Ctrl-C handler. Idempotent; safe to call from `main` only.
#[expect(unsafe_code, reason = "libc::signal has no safe equivalent in std")]
pub(crate) fn catch_interrupts() {
    // SAFETY: `on_sigint` is an `extern "C"` function that only stores into
    // an `AtomicBool`, which is async-signal-safe. Installing a handler is
    // sound as long as the handler itself is, and this one does nothing
    // else. Called from `main` before any thread is spawned.
    #[cfg(unix)]
    unsafe {
        extern "C" fn on_sigint(_: libc::c_int) {
            INTERRUPTED.store(true, Ordering::SeqCst);
        }
        libc::signal(libc::SIGINT, on_sigint as *const () as libc::sighandler_t);
    }
}

pub(crate) struct Spawn<'a> {
    /// Program and arguments, already sudo-wrapped if the step asked for it.
    pub argv: &'a [String],
    pub cwd: Option<&'a Path>,
    pub env: &'a BTreeMap<String, String>,
    /// Fed to the child's stdin and closed. Only sudo's `-S` uses it.
    pub stdin: Option<&'a str>,
    pub timeout: Duration,
    /// Copy output to the terminal as it arrives as well as capturing it.
    pub stream: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct Output {
    pub rc: i32,
    pub stdout: String,
    pub stderr: String,
    pub how: How,
}

/// Why the child stopped. `Exited` is the only one that makes `rc` meaningful.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum How {
    Exited,
    TimedOut,
    Interrupted,
}

/// Spawn, capture, and wait — killing the whole group on timeout or Ctrl-C.
///
/// The lossy view, for the runner and for `register`.
pub(crate) fn run(s: Spawn<'_>) -> std::io::Result<Output> {
    let raw = run_raw(&s)?;
    Ok(Output {
        rc: raw.rc,
        stdout: String::from_utf8_lossy(&raw.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&raw.stderr).into_owned(),
        how: raw.how,
    })
}

/// The exact-bytes view, for reading state.
///
/// `file` compares a target against itself byte for byte, and a lossy String
/// makes a binary file differ from itself forever. This is the same spawn,
/// the same process group and the same watchdog — only the last step differs.
pub(crate) fn capture(s: Spawn<'_>) -> std::io::Result<Raw> {
    run_raw(&s)
}

pub(crate) struct Raw {
    pub rc: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub how: How,
}

impl Raw {
    /// stderr as text, which is all any caller wants of it.
    pub(crate) fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }
}

fn run_raw(s: &Spawn<'_>) -> std::io::Result<Raw> {
    let (program, args) = s
        .argv
        .split_first()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "empty command"))?;

    let mut cmd = Command::new(program);
    cmd.args(args)
        .envs(s.env)
        .stdin(if s.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(dir) = s.cwd {
        cmd.current_dir(dir);
    }
    own_process_group(&mut cmd);

    let mut child = cmd.spawn()?;
    let pid = child.id();

    if let Some(text) = s.stdin
        && let Some(mut pipe) = child.stdin.take()
    {
        // A child that never reads stdin makes this a broken pipe, which is
        // not an error here — sudo may already have a cached credential.
        #[expect(
            clippy::let_underscore_must_use,
            reason = "a broken pipe here is the documented case above"
        )]
        let _ = pipe.write_all(text.as_bytes());
    }

    let out = drain(child.stdout.take(), s.stream, false);
    let err = drain(child.stderr.take(), s.stream, true);

    let how = wait_for(&mut child, pid, s.timeout);
    let rc = match how {
        How::Exited => exit_code(&mut child),
        // A killed child has no exit status worth reporting. 124 is what
        // timeout(1) uses; 130 is the shell's convention for SIGINT.
        How::TimedOut => 124,
        How::Interrupted => 130,
    };

    Ok(Raw {
        rc,
        stdout: out.join().unwrap_or_default(),
        stderr: err.join().unwrap_or_default(),
        how,
    })
}

/// Wait, but wake up often enough to notice a Ctrl-C or a blown deadline.
///
/// The child is moved nowhere: a helper thread owns the blocking `wait`, and
/// this thread polls a channel. 100ms of latency on a kill is invisible to a
/// person and costs nothing; the alternative is a signal-driven wait that has
/// to be correct under EINTR.
fn wait_for(child: &mut Child, pid: u32, timeout: Duration) -> How {
    let deadline = Instant::now() + timeout;
    let tick = Duration::from_millis(100);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return How::Exited,
            Ok(None) => {}
            Err(_) => return How::Exited,
        }
        if interrupted() {
            kill_group(pid);
            // Reaping a process we just killed. It is going to exit, and its
            // status is not the answer -- `How` already is.
            #[expect(clippy::let_underscore_must_use, reason = "reaping a killed child")]
            let _ = child.wait();
            return How::Interrupted;
        }
        let now = Instant::now();
        if now >= deadline {
            kill_group(pid);
            #[expect(clippy::let_underscore_must_use, reason = "reaping a killed child")]
            let _ = child.wait();
            return How::TimedOut;
        }
        std::thread::sleep(tick.min(deadline - now));
    }
}

fn exit_code(child: &mut Child) -> i32 {
    match child.wait() {
        Ok(st) => st.code().unwrap_or_else(|| signal_code(st)),
        Err(_) => -1,
    }
}

#[cfg(unix)]
fn signal_code(st: std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    // The shell's convention, so `failed_when: result.rc == 139` reads the
    // way an operator expects for a segfault.
    st.signal().map_or(-1, |s| 128 + s)
}

#[cfg(not(unix))]
fn signal_code(_st: std::process::ExitStatus) -> i32 {
    -1
}

/// Read a pipe to the end on its own thread, optionally teeing it to the
/// terminal. Two threads, because a child that fills the stderr pipe while
/// provision reads stdout would deadlock (spec §6.1 promises full capture).
fn drain(pipe: Option<impl Read + Send + 'static>, stream: bool, is_err: bool) -> Reader {
    let Some(mut pipe) = pipe else {
        return Reader(None);
    };
    let (tx, rx) = mpsc::channel();
    let h = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            match pipe.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                // `read` never reports more than it was given, so the slice
                // is always in range; `get` says so without asking a reader
                // to know that.
                Ok(n) => {
                    let Some(got) = chunk.get(..n) else { break };
                    if stream {
                        let text = String::from_utf8_lossy(got);
                        if is_err {
                            eprint!("{text}");
                        } else {
                            print!("{text}");
                        }
                    }
                    buf.extend_from_slice(got);
                }
            }
        }
        // The receiver is gone only when the run is already unwinding.
        #[expect(
            clippy::let_underscore_must_use,
            reason = "nothing to do if the reader hung up"
        )]
        let _ = tx.send(buf);
    });
    Reader(Some((h, rx)))
}

struct Reader(Option<(std::thread::JoinHandle<()>, mpsc::Receiver<Vec<u8>>)>);

impl Reader {
    fn join(self) -> Option<Vec<u8>> {
        let (h, rx) = self.0?;
        let text = rx.recv().ok();
        #[expect(
            clippy::let_underscore_must_use,
            reason = "the output is already in hand"
        )]
        let _ = h.join();
        text
    }
}

// ── platform ──────────────────────────────────────────────────────────────

#[cfg(unix)]
fn own_process_group(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    cmd.process_group(0);
}

#[cfg(windows)]
fn own_process_group(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    cmd.creation_flags(CREATE_NEW_PROCESS_GROUP);
}

/// Kill the child *and everything it started*. TERM first so a package
/// manager mid-write gets to finish its transaction, then KILL.
#[cfg(unix)]
#[expect(
    unsafe_code,
    reason = "killing a process *group* is not in std; Command::process_group is"
)]
fn kill_group(pid: u32) {
    let pgid = pid as libc::pid_t;
    // SAFETY: `killpg` takes a pgid and a signal and touches no memory. The
    // pgid is this process's own child, put in its own group by
    // `own_process_group` at spawn, so the worst a stale one can do is fail
    // with ESRCH -- which the loop below reads as "already gone".
    unsafe {
        libc::killpg(pgid, libc::SIGTERM);
    }
    for _ in 0..20 {
        std::thread::sleep(Duration::from_millis(100));
        // ESRCH means the group is gone; anything else means it is still there.
        // SAFETY: signal 0 is the existence check; it delivers nothing.
        if unsafe { libc::killpg(pgid, 0) } != 0 {
            return;
        }
    }
    // SAFETY: as above. TERM was given 2s to be polite; this is the end of it.
    unsafe {
        libc::killpg(pgid, libc::SIGKILL);
    }
}

#[cfg(windows)]
fn kill_group(pid: u32) {
    // Windows is phase 3. `taskkill /T` is the documented way to end a tree
    // and is present on every supported version.
    let _ = Command::new("taskkill")
        .args(["/T", "/F", "/PID", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}
