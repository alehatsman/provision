//! The phase 1 gate: execution.
//!
//! Every action ships with a test that runs it twice on a scratch filesystem
//! and asserts `changed` then `ok` (spec §6). For `shell` that means the two
//! gates it can declare — `unless` and `creates` — and for `assert` it means
//! that running twice changes nothing at all.
//!
//! The whole file is unix-only, and gated in one place rather than test by
//! test. Not an oversight about coverage: every fixture here is POSIX-shaped
//! — `sudo`, `/etc`, systemd units, `#!/bin/sh` stand-ins on PATH, `0644` —
//! so gating individually would produce the same zero Windows coverage for
//! thirty times the diff. What it buys is that
//! `cargo check --target x86_64-pc-windows-gnu --all-targets` stays clean, so
//! a Windows build break is caught here instead of on the Windows box. When
//! that box joins the loop, this file splits: the platform-neutral half
//! (parsing, tags, exit codes) loses the gate, and the half that needs a
//! POSIX filesystem keeps it.
//!
//! # A test runs everywhere, or it says what it needs
//!
//! There is no third category, and the third category is what kept appearing.
//! A test that quietly assumes the machine it was written on does not fail
//! honestly — it passes for its author and fails for everyone else, and the
//! failure looks like a bug in provision rather than in the test. Found in one
//! sweep: a rendered `{{ os }}` asserted against the literal `linux`; a guard
//! that only failed on three hostnames the author owns; `chown root:root`,
//! where macOS has no `root` group; `stat -c`, which is GNU's; a `pkg` test
//! that stubbed `dpkg-query` but let `apt-get` resolve from the host's PATH;
//! and a `defaults` fixture that converged the real preferences of whoever ran
//! the suite. None was a defect in the tool.
//!
//! So: derive the expected value from the same `cfg!` the code uses — see
//! [`os_fact`] and [`root_group`] — or stub the dependency into the test's own
//! tempdir, or declare the requirement with `#[ignore = "needs …"]` as the
//! container tests below do. Reach for `#[ignore]` last: an ignored test is a
//! test nobody runs. A `cfg` gate is right only when the behaviour under test
//! genuinely differs by platform, not when the assertion is merely
//! inconvenient to write portably.
//!
//! This is a tool for converging machines that are not each other. Its own
//! suite running on exactly one kind of machine is how all six got in.
//! Unix only, and deliberately: most of what follows is modes, `chown`,
//! `stat` and sudo. The engine underneath — verdicts, gates, registers, the
//! JSON shape, the deadline — is not platform behaviour and runs on both in
//! `tests/apply_cross_platform.rs` (#26). Before that file existed this
//! gate was the broad form of exactly what the comment above warns about.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Durations are the only thing in the output that changes between runs, in
/// the terminal form and in the JSON one.
macro_rules! snapshot {
    ($name:expr, $text:expr) => {
        insta::with_settings!(
            {filters => vec![
                (r#""duration_ms":\d+"#, r#""duration_ms":0"#),
                (r"\d+ms", "[t]"),
                (r"\d+\.\d+s", "[t]"),
            ]},
            { insta::assert_snapshot!($name, $text) }
        )
    };
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/apply")
        .join(name)
}

/// The `os` fact, derived the way `src/facts.rs` derives it.
///
/// A `cfg!` there and a `cfg!` here, both compiled for the same target, so a
/// fixture that interpolates `{{ os }}` can still be asserted exactly —
/// without the test deciding in advance which machine it is running on. This
/// file is `#![cfg(unix)]`, so `windows` is not reachable from it.
fn os_fact() -> &'static str {
    if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "android") {
        "android"
    } else {
        "linux"
    }
}

/// Root's group. `root` on Linux, `wheel` on macOS — there is no name that
/// works on both, so anything naming it has to ask.
fn root_group() -> &'static str {
    if cfg!(target_os = "macos") {
        "wheel"
    } else {
        "root"
    }
}

/// Run a command with `$PROVISION_SCRATCH` pointed at a directory the plan
/// owns. The plans read it through `env.PROVISION_SCRATCH`.
fn run_in(scratch: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_provision"))
        .args(args)
        .env("PROVISION_SCRATCH", scratch)
        .env("NO_COLOR", "1")
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")))
        .output()
        .expect("provision failed to start")
}

fn apply(scratch: &Path, plan: &str, extra: &[&str]) -> (i32, String) {
    let path = fixture(plan);
    let mut args = vec!["apply", path.to_str().expect("fixture paths are UTF-8")];
    args.extend_from_slice(extra);
    let out = run_in(scratch, &args);
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
    )
}

/// The status column for one step, by name. Lines are
/// `  {glyph} {name:<48} {label}  {duration}`, so the label starts at a fixed
/// offset and the duration is the last word.
fn verdict(out: &str, name: &str) -> String {
    let line = line_for(out, name);
    let tail: String = line.chars().skip(4 + 48).collect();
    let mut words: Vec<&str> = tail.split_whitespace().collect();
    words.pop();
    words.join(" ")
}

// A missing line is this helper's assertion failure; panicking with the
// whole output is how the test says what it expected.
#[expect(clippy::panic, reason = "a test helper's assertion failure")]
fn line_for<'a>(out: &'a str, name: &str) -> &'a str {
    out.lines()
        .find(|l| l.contains(name))
        .unwrap_or_else(|| panic!("no line for {name}:\n{out}"))
}

// ── the gate ──────────────────────────────────────────────────────────────

#[test]
fn the_scratch_home_plan_applies_twice_changed_then_skipped() {
    let dir = tempfile::tempdir().unwrap();

    let (code, first) = apply(dir.path(), "scratch_home.yml", &[]);
    assert_eq!(code, 0, "{first}");
    assert!(
        line_for(&first, "scratch directory").contains("changed"),
        "{first}"
    );
    assert!(
        line_for(&first, "identity key").contains("changed"),
        "{first}"
    );
    assert!(
        line_for(&first, "identity key present").contains("ok"),
        "{first}"
    );
    assert!(
        dir.path().join(".ssh").is_dir(),
        "the directory was not made"
    );
    assert!(
        dir.path().join("id_ed25519").is_file(),
        "the key was not written"
    );

    let (code, second) = apply(dir.path(), "scratch_home.yml", &[]);
    assert_eq!(code, 0, "{second}");
    // `creates` and `unless` are the two ways a shell step declares that it is
    // already done. Both must fire on the second run and neither on the first.
    assert!(
        line_for(&second, "scratch directory").contains("creates exists"),
        "{second}"
    );
    assert!(
        line_for(&second, "identity key").contains("unless"),
        "{second}"
    );
    assert!(
        line_for(&second, "identity key present").contains("ok"),
        "{second}"
    );
    assert!(
        !second.contains("changed"),
        "nothing should change twice:\n{second}"
    );
}

// Spec §3: files resolve relative to the file that names them, never the cwd.
// `creates` is one of them, so one plan must reach one verdict from anywhere.
#[test]
fn creates_resolves_against_the_plan_not_the_invocation_directory() {
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    std::fs::write(sub.join("hit"), "").unwrap();
    std::fs::write(
        sub.join("p.yml"),
        "- name: Gated on a file beside this plan\n  shell: \"true\"\n  creates: ./hit\n",
    )
    .unwrap();

    let verdict_from = |cwd: &Path, plan: &str| {
        let out = Command::new(env!("CARGO_BIN_EXE_provision"))
            .args(["plan", plan])
            .env("NO_COLOR", "1")
            .current_dir(cwd)
            .output()
            .expect("provision failed to start");
        verdict(
            &(String::from_utf8_lossy(&out.stdout).into_owned()
                + &String::from_utf8_lossy(&out.stderr)),
            "Gated on a file beside this plan",
        )
    };

    let beside = verdict_from(&sub, "p.yml");
    let above = verdict_from(dir.path(), "sub/p.yml");
    assert_eq!(beside, "skipped creates exists");
    assert_eq!(above, beside, "same plan, same file, two verdicts");
}

// Spec §3: `creates` asks about a file at the moment a step runs, so its `~`
// is the process's home even when the plan shadows `home`.
#[test]
fn creates_tilde_is_the_process_home_when_the_plan_shadows_home() {
    let dir = tempfile::tempdir().unwrap();
    let real_home = dir.path().join("real");
    let other_home = dir.path().join("other");
    std::fs::create_dir(&real_home).unwrap();
    std::fs::create_dir(&other_home).unwrap();
    std::fs::write(real_home.join("hit"), "").unwrap();
    let plan = dir.path().join("p.yml");
    std::fs::write(
        &plan,
        format!(
            "- name: shadowed home\n  vars:\n    home: {}\n\n- name: Gated on the process home\n  shell: \"true\"\n  creates: ~/hit\n",
            other_home.display()
        ),
    )
    .unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_provision"))
        .args(["plan", plan.to_str().unwrap()])
        .env("NO_COLOR", "1")
        .env("HOME", &real_home)
        .output()
        .expect("provision failed to start");
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        verdict(&text, "Gated on the process home"),
        "skipped creates exists",
        "{text}"
    );
}

#[test]
fn plan_probes_the_same_gates_apply_uses() {
    let dir = tempfile::tempdir().unwrap();
    let path = fixture("scratch_home.yml");
    let plan = |d: &Path| {
        let out = run_in(d, &["plan", path.to_str().unwrap()]);
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
        )
    };

    // Nothing has run, so the closing assert cannot hold yet: exit 1. Spec
    // §6.7 — plan reports that and keeps walking, so all three steps are here.
    let (code, before) = plan(dir.path());
    assert_eq!(code, 1, "{before}");
    assert!(
        line_for(&before, "scratch directory").contains("would run"),
        "{before}"
    );
    assert!(
        line_for(&before, "identity key present").contains("FAILED"),
        "{before}"
    );
    assert!(before.contains("3 steps"), "{before}");
    assert!(
        !dir.path().join(".ssh").exists(),
        "plan must not create anything"
    );

    apply(dir.path(), "scratch_home.yml", &[]);

    let (code, after) = plan(dir.path());
    assert_eq!(code, 0, "a converged plan has nothing to do:\n{after}");
    assert!(
        line_for(&after, "scratch directory").contains("creates exists"),
        "{after}"
    );
}

// ── verdicts ──────────────────────────────────────────────────────────────

#[test]
fn each_modifier_reaches_its_own_verdict() {
    let dir = tempfile::tempdir().unwrap();
    let (code, out) = apply(dir.path(), "verdicts.yml", &[]);
    assert_eq!(code, 0, "{out}");

    // Spec §6.1, D20: no gate means no claim about state, so the exit code is
    // the whole contract and `true` succeeded.
    assert_eq!(verdict(&out, "ungated shell"), "ok", "{out}");
    assert!(line_for(&out, "work is done").contains("unless"), "{out}");
    assert_eq!(verdict(&out, "changed_when decides"), "changed", "{out}");
    // The register held a real value, so `when` was answerable at apply time.
    assert_eq!(verdict(&out, "reads the register"), "ok", "{out}");
    // `failed_when` forgave exit 3, so the step is a success.
    assert_eq!(verdict(&out, "forgives a non-zero"), "ok", "{out}");
}

#[test]
fn a_step_that_would_run_makes_plan_exit_two() {
    // D15: anything that is not `ok` or `skipped` is something to do. An
    // ungated step is `would run` since D20, which is in that same bucket, so
    // no exit code moved — only the word. `unknown` still counts for the same
    // reason and is asserted where it now comes from: `pkg latest`
    // (`pkg_plan_names_what_it_would_install_and_admits_what_it_cannot_know`),
    // a `git` branch ref, and `defaults` off macOS.
    let dir = tempfile::tempdir().unwrap();
    let path = fixture("verdicts.yml");
    let out = run_in(dir.path(), &["plan", path.to_str().unwrap()]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(2), "{text}");
    assert_eq!(verdict(&text, "ungated shell"), "would run", "{text}");
}

#[test]
fn plan_no_probe_claims_nothing_and_exits_zero() {
    let dir = tempfile::tempdir().unwrap();
    let path = fixture("verdicts.yml");
    let out = run_in(
        dir.path(),
        &["plan", "--plan-no-probe", path.to_str().unwrap()],
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "{text}");
    assert!(text.contains("would run (unprobed)"), "{text}");
    assert!(
        !text.contains("unknown"),
        "nothing was probed, so nothing is unknown:\n{text}"
    );
}

// ── the bare step (spec §6.1, D20) ────────────────────────────────────────
//
// A step with no `unless`, `creates` or `changed_when` declares nothing about
// state, so its exit code is the whole of what it promised. That is the task
// and CI shape, and it used to cost a `changed_when: false` per step to say.

#[test]
fn a_bare_shell_step_is_ok_on_exit_zero() {
    let dir = tempfile::tempdir().unwrap();
    let (code, out) = apply(dir.path(), "bare_step.yml", &[]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(verdict(&out, "bare shell that exits zero"), "ok", "{out}");
}

#[test]
fn a_bare_cmd_step_is_ok_on_exit_zero() {
    // `cmd` takes §6.1's semantics by reference (§6.2), so it has to be
    // asserted separately or the reference is the only thing holding it.
    let dir = tempfile::tempdir().unwrap();
    let (code, out) = apply(dir.path(), "bare_step.yml", &[]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(verdict(&out, "bare cmd that exits zero"), "ok", "{out}");
}

#[test]
fn a_bare_shell_step_fails_on_a_non_zero_exit() {
    // The half of the contract that did not move: `failed_when` already
    // defaulted to `result.rc != 0`. Asserted so that "a bare step reads ok"
    // cannot quietly become "a bare step always passes".
    let dir = tempfile::tempdir().unwrap();
    let (code, out) = apply(dir.path(), "bare_step_fails.yml", &[]);
    assert_eq!(code, 1, "{out}");
    assert_eq!(
        verdict(&out, "bare shell that exits non-zero"),
        "FAILED",
        "{out}"
    );
}

#[test]
fn a_bare_step_would_run_under_plan() {
    // Plan has not run it, so it cannot claim `ok`. But it carries no gate, so
    // plan does know it will run — which is more than `unknown` said.
    let dir = tempfile::tempdir().unwrap();
    let path = fixture("bare_step.yml");
    let out = run_in(dir.path(), &["plan", path.to_str().unwrap()]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(2), "{text}");
    assert_eq!(
        verdict(&text, "bare shell that exits zero"),
        "would run",
        "{text}"
    );
    assert!(
        !text.contains("unknown"),
        "an ungated step is no longer unknown:\n{text}"
    );
}

// ── failure ───────────────────────────────────────────────────────────────

#[test]
fn a_failure_stops_the_run_and_shows_the_stderr_tail() {
    let dir = tempfile::tempdir().unwrap();
    let (code, out) = apply(dir.path(), "failure.yml", &[]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("FAILED"), "{out}");
    assert!(out.contains("sed: can't read /etc/pacman.conf"), "{out}");
    assert!(out.contains("exit 2"), "{out}");
    assert!(
        !out.contains("Never reached"),
        "the run must stop at the failure:\n{out}"
    );
}

// Spec §8 `--keep-going`. The point of the flag is the list: on a bare
// machine the first run should say everything that is broken, not the first
// thing. `failure.yml`'s third step is named "Never reached" for the default
// behaviour; with the flag it is reached.
#[test]
fn keep_going_carries_on_past_a_failed_step() {
    let dir = tempfile::tempdir().unwrap();
    let (code, out) = apply(dir.path(), "failure.yml", &["--keep-going"]);
    assert_eq!(code, 1, "a kept-going run still fails:\n{out}");
    assert!(
        out.contains("Never reached"),
        "the run stopped anyway:\n{out}"
    );
    assert!(out.contains("sed: can't read /etc/pacman.conf"), "{out}");
}

#[test]
fn keep_going_reports_every_failure_and_the_register_of_one() {
    let dir = tempfile::tempdir().unwrap();
    let (code, out) = apply(dir.path(), "keep_going.yml", &["--keep-going"]);
    assert_eq!(code, 1, "{out}");
    assert_eq!(
        out.matches("FAILED").count(),
        2,
        "both failures should show:\n{out}"
    );
    // The register holds the failed step's own result, so a later `when`
    // reading it sees what happened rather than a placeholder.
    assert_eq!(
        verdict(&out, "Reads the failed step's register"),
        "ok",
        "{out}"
    );
    assert_eq!(verdict(&out, "Last step still runs"), "ok", "{out}");
    assert!(
        out.contains("2 failed"),
        "the summary should count both:\n{out}"
    );
}

// Without the flag the same plan stops at the first of the two, which is what
// makes the test above mean something.
#[test]
fn without_keep_going_the_same_plan_stops_at_the_first_failure() {
    let dir = tempfile::tempdir().unwrap();
    let (code, out) = apply(dir.path(), "keep_going.yml", &[]);
    assert_eq!(code, 1, "{out}");
    assert_eq!(out.matches("FAILED").count(), 1, "{out}");
    assert!(!out.contains("Last step still runs"), "{out}");
}

// `an_unimplemented_action_says_which_phase_brings_it` lived here through
// phase 2, repointed each time an action landed: file, then pkg. All seven
// actions now exist, so `NotYet` is unreachable from a plan and the test has
// nothing left to assert. Retired at the phase 2 gate rather than quietly
// deleted — the machinery it covered is still in the runner, for the next
// action that arrives ahead of its implementation.

// ── retry and timeout ─────────────────────────────────────────────────────

#[test]
fn retry_reruns_until_the_step_passes() {
    let dir = tempfile::tempdir().unwrap();
    let (code, out) = apply(dir.path(), "retry.yml", &[]);
    assert_eq!(code, 0, "{out}");
    assert!(
        line_for(&out, "third attempt").contains("attempt 3/3"),
        "{out}"
    );
    assert!(line_for(&out, "third attempt").contains("changed"), "{out}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("tries"))
            .unwrap()
            .trim(),
        "3"
    );
}

#[test]
fn a_timeout_kills_the_children_not_only_the_shell() {
    let dir = tempfile::tempdir().unwrap();
    let (code, out) = apply(dir.path(), "timeout.yml", &[]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("timed out after 2"), "{out}");

    let pid: i32 = std::fs::read_to_string(dir.path().join("child.pid"))
        .expect("the step never wrote its child's pid")
        .trim()
        .parse()
        .unwrap();
    // `kill -0` succeeds only while the process is alive. A bare child.kill()
    // would have left this `sleep 300` running for five minutes.
    let alive = Command::new("kill")
        .args(["-0", &pid.to_string()])
        .output()
        .is_ok_and(|o| o.status.success());
    assert!(
        !alive,
        "pid {pid} outlived the process group it was killed with"
    );
}

// A child that ignores TERM is killed anyway, on the grace period's clock.
// `kill_group` used to reap the direct child with a blocking `wait` *before*
// the grace loop, which put the only thing that guarantees the child dies —
// the KILL at the end of that loop — behind the wait for it to die. A step
// that trapped TERM then ran to completion with `timeout`, `--deadline` and
// Ctrl-C all unenforceable against it.
#[test]
fn a_step_that_traps_term_is_still_killed() {
    let dir = tempfile::tempdir().unwrap();
    let started = std::time::Instant::now();
    let (code, out) = apply(dir.path(), "trapped_term.yml", &[]);
    let elapsed = started.elapsed();
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("timed out after 1"), "{out}");
    assert!(
        elapsed < std::time::Duration::from_secs(30),
        "the grace period never escalated to KILL: {elapsed:?}\n{out}"
    );
    assert!(
        !out.contains("Must not run after the timeout"),
        "the walk did not stop:\n{out}"
    );
    assert!(
        !child_alive(dir.path()),
        "the step that ignored TERM outlived its timeout"
    );
}

/// Whether the pid a fixture wrote to `child.pid` is still running.
fn child_alive(dir: &Path) -> bool {
    let pid = std::fs::read_to_string(dir.join("child.pid"))
        .expect("the step never wrote its child's pid");
    Command::new("kill")
        .args(["-0", pid.trim()])
        .output()
        .is_ok_and(|o| o.status.success())
}

// Spec §4: `timeout` bounds the step, and a step whose shell has exited is not
// finished while a child it started still holds the output pipe. The clock
// used to stop at the shell's exit, and the run then waited on the pipe with
// nothing watching it — 30s, reported `ok`.
#[test]
fn a_timeout_covers_a_child_still_holding_the_pipe() {
    let dir = tempfile::tempdir().unwrap();
    let started = std::time::Instant::now();
    let (code, out) = apply(dir.path(), "held_pipe_timeout.yml", &[]);
    let elapsed = started.elapsed();
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("timed out after 1"), "{out}");
    assert!(
        elapsed < std::time::Duration::from_secs(10),
        "waited on the pipe past the timeout: {elapsed:?}\n{out}"
    );
    assert!(
        !out.contains("Must not run after the timeout"),
        "the walk did not stop:\n{out}"
    );
    assert!(
        !child_alive(dir.path()),
        "the child holding the pipe outlived the timeout"
    );
}

// The same held pipe under a TERM (spec §10, D19): the signal used to go
// unread until the pipe closed, so a cancelled job ran on for 30s and exited 0.
#[test]
fn sigterm_reaches_a_child_still_holding_the_pipe() {
    let dir = tempfile::tempdir().unwrap();
    let path = fixture("held_pipe_terminate.yml");
    let started = std::time::Instant::now();
    let child = Command::new(env!("CARGO_BIN_EXE_provision"))
        .args(["apply", path.to_str().expect("fixture paths are UTF-8")])
        .env("PROVISION_SCRATCH", dir.path())
        .env("NO_COLOR", "1")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("provision failed to start");

    // Wait for the step's background child to exist, rather than sleeping a
    // fixed 600ms and hoping. The old guess raced provision's own startup:
    // on a loaded host the TERM landed before the handler was installed, the
    // default disposition killed the run outright, and the test failed on
    // `status.code()` being None rather than on anything it means to assert
    // (#64). The plan writes the pid, so seeing the file proves the step is
    // running and the handler is up.
    let pid = dir.path().join("child.pid");
    let waited = std::time::Instant::now();
    while !pid.exists() {
        assert!(
            waited.elapsed() < std::time::Duration::from_secs(30),
            "the step never started its child"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    // The pid is the last thing the shell writes, so let it finish exiting:
    // the stop under test is a TERM arriving once only the background child
    // still holds the pipe.
    std::thread::sleep(std::time::Duration::from_millis(200));
    #[expect(
        clippy::let_underscore_must_use,
        reason = "the child's own output is the assertion"
    )]
    let _ = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status();

    let out = child.wait_with_output().expect("child was spawned");
    let elapsed = started.elapsed();
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);

    assert_eq!(out.status.code(), Some(143), "{text}");
    assert!(
        elapsed < std::time::Duration::from_secs(10),
        "waited on the pipe past the TERM: {elapsed:?}\n{text}"
    );
    assert!(
        !text.contains("Must not run after a TERM"),
        "kept going past the TERM:\n{text}"
    );
    assert!(
        !child_alive(dir.path()),
        "the child holding the pipe outlived the TERM"
    );
}

// Spec §10: SIGTERM stops the run whether or not a child is running when it
// arrives. The handler only records the signal, and the wait on a child was
// the one place that read it — so a TERM during steps that spawn nothing, the
// `file` writes here, went unread: every remaining step ran and the run exited
// 0. The plan is generated long enough that the walk is still writing when the
// signal lands; the marker step says the walk has started, so the handler is
// installed before anything is sent.
#[test]
fn sigterm_between_steps_that_spawn_nothing_stops_the_run() {
    use std::fmt::Write as _;
    const FILES: usize = 20_000;

    let dir = tempfile::tempdir().unwrap();
    let written = dir.path().join("out");
    std::fs::create_dir(&written).unwrap();
    let started = dir.path().join("started");
    let mut plan = String::new();
    writeln!(
        plan,
        "- name: Marks the walk as started\n  shell: touch '{}'\n  changed_when: \"false\"",
        started.display()
    )
    .unwrap();
    for i in 0..FILES {
        writeln!(
            plan,
            "- file: {{path: '{}/f{i}', content: x}}",
            written.display()
        )
        .unwrap();
    }
    let path = dir.path().join("many_files.yml");
    std::fs::write(&path, plan).unwrap();

    let child = Command::new(env!("CARGO_BIN_EXE_provision"))
        .args(["apply", path.to_str().expect("temp paths are UTF-8")])
        .env("NO_COLOR", "1")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("provision failed to start");

    let waited = std::time::Instant::now();
    while !started.exists() {
        assert!(
            waited.elapsed() < std::time::Duration::from_secs(30),
            "the walk never started"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    #[expect(
        clippy::let_underscore_must_use,
        reason = "the child's own output is the assertion"
    )]
    let _ = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status();

    let out = child.wait_with_output().expect("child was spawned");
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    let tail: String = text.lines().rev().take(5).collect::<Vec<_>>().join("\n");

    assert_eq!(out.status.code(), Some(143), "{tail}");
    assert!(text.contains("interrupted"), "{tail}");
    let count = std::fs::read_dir(&written).unwrap().count();
    assert!(
        count < FILES,
        "every step ran after the TERM ({count} files)"
    );
}

// The same stop under `plan` (spec §10). A gate is a child in its own process
// group, so the terminal's signal never reaches it, and `plan` installed no
// handler: a TERM killed provision where it stood and left a hung gate
// running, unowned.
#[test]
fn sigterm_during_plan_kills_the_gate() {
    let dir = tempfile::tempdir().unwrap();
    let path = fixture("plan_gate_terminate.yml");
    let child = Command::new(env!("CARGO_BIN_EXE_provision"))
        .args(["plan", path.to_str().expect("fixture paths are UTF-8")])
        .env("PROVISION_SCRATCH", dir.path())
        .env("NO_COLOR", "1")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("provision failed to start");

    let pid_file = dir.path().join("child.pid");
    let waited = std::time::Instant::now();
    while !pid_file.exists() {
        assert!(
            waited.elapsed() < std::time::Duration::from_secs(30),
            "the gate never started"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    #[expect(
        clippy::let_underscore_must_use,
        reason = "the child's own output is the assertion"
    )]
    let _ = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status();

    let out = child.wait_with_output().expect("child was spawned");
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);

    assert_eq!(out.status.code(), Some(143), "{text}");
    assert!(text.contains("interrupted"), "{text}");
    assert!(
        !child_alive(dir.path()),
        "the gate outlived the TERM that stopped plan"
    );
}

// D22: `rc: 124` alone cannot tell provision's own kill from a step that
// wraps its own command in `timeout(1)` and legitimately exits 124.
#[test]
fn timed_out_distinguishes_a_kill_from_the_steps_own_124() {
    let dir = tempfile::tempdir().unwrap();
    let (code, out) = apply(dir.path(), "timeout_register.yml", &["--keep-going"]);
    assert_eq!(code, 1, "the killed step still fails the run:\n{out}");
    assert_eq!(
        verdict(&out, "Reads both registers"),
        "ok",
        "killed.timed_out and own_124.timed_out should have told them apart:\n{out}"
    );
}

// D22's amendment: the stream has to say the same thing the register does.
// Reporting `rc: null` here made one step in one run carry two exit codes,
// one for `failed_when` and another for the CI runner reading `--json`.
#[test]
fn the_json_stream_reports_a_killed_steps_rc_and_who_killed_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = fixture("timeout_register.yml");
    let out = run_in(
        dir.path(),
        &["apply", path.to_str().unwrap(), "--json", "--keep-going"],
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let rows: Vec<serde_json::Value> = stdout
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let step = |name: &str| -> serde_json::Value {
        rows.iter()
            .find(|r| r["event"] == "step" && r["name"] == name)
            .unwrap_or_else(|| panic!("no step event for `{name}`:\n{stdout}"))
            .clone()
    };

    let killed = step("Provision kills this one");
    assert_eq!(killed["rc"], 124, "{stdout}");
    assert_eq!(killed["timed_out"], true, "{stdout}");

    // The other half of D22: a command that exits 124 by itself reports the
    // same `rc` and must not claim the clock killed it.
    let own = step("This one exits 124 on its own");
    assert_eq!(own["rc"], 124, "{stdout}");
    assert!(own.get("timed_out").is_none(), "{stdout}");
}

// ── output forms ──────────────────────────────────────────────────────────

#[test]
fn json_emits_one_object_per_step_and_a_summary() {
    let dir = tempfile::tempdir().unwrap();
    let path = fixture("verdicts.yml");
    let out = run_in(dir.path(), &["apply", path.to_str().unwrap(), "--json"]);
    let stdout = String::from_utf8_lossy(&out.stdout);

    let rows: Vec<serde_json::Value> = stdout
        .lines()
        .map(|l| serde_json::from_str(l).expect(l))
        .collect();
    assert_eq!(rows.last().unwrap()["event"], "summary");

    let steps: Vec<&serde_json::Value> = rows.iter().filter(|r| r["event"] == "step").collect();
    assert_eq!(steps.len(), 5, "{stdout}");
    assert_eq!(steps[0]["status"], "ok");
    assert_eq!(steps[1]["status"], "skipped");
    assert_eq!(steps[1]["reason"], "unless");
    assert_eq!(steps[2]["status"], "changed");
    // Spec §9.3: every step carries the file and line it came from.
    assert!(steps[0]["line"].as_u64().unwrap() > 0, "{stdout}");
    assert!(
        steps[0]["file"].as_str().unwrap().ends_with("verdicts.yml"),
        "{stdout}"
    );

    // Human output moved to stderr so stdout stays parseable.
    assert!(String::from_utf8_lossy(&out.stderr).contains("ungated shell"));

    snapshot!("json", stdout);
}

// Spec §9.3, phase 5b. The three keys used to arrive only with a failure,
// which made the stream useless for the thing it is for: a CI runner reading
// one result per step. They are the step's *own command's*, so a typed action
// carries none -- its `rc` is invented so `changed_when` and `register` mean
// the same thing everywhere, and is not an exit code anyone should read --
// and a skipped step carries none either, because a gate decides whether the
// step runs and its result is not the step's.
#[test]
fn json_carries_rc_and_output_for_every_step_that_ran_a_command() {
    let dir = tempfile::tempdir().unwrap();
    let path = fixture("json_step_output.yml");
    let out = run_in(
        dir.path(),
        &["apply", path.to_str().unwrap(), "--json", "--keep-going"],
    );
    let steps: Vec<serde_json::Value> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).expect(l))
        .filter(|r| r["event"] == "step")
        .collect();
    assert_eq!(steps.len(), 4, "{steps:?}");

    // Succeeded: all three present, in full, without a failure to prompt them.
    assert_eq!(steps[0]["status"], "ok");
    assert_eq!(steps[0]["rc"], 0);
    assert_eq!(steps[0]["stdout"], "out-ok\n");
    assert_eq!(steps[0]["stderr"], "err-ok\n");

    // Failed: the same three, with the command's own exit code.
    assert_eq!(steps[1]["status"], "failed");
    assert_eq!(steps[1]["rc"], 4);
    assert_eq!(steps[1]["stdout"], "out-bad\n");
    assert_eq!(steps[1]["stderr"], "err-bad\n");

    // A typed action and a skipped step ran no command of their own.
    assert_eq!(steps[2]["status"], "changed");
    for key in ["rc", "stdout", "stderr"] {
        assert!(
            steps[2][key].is_null(),
            "typed action carried {key}: {steps:?}"
        );
        assert!(
            steps[3][key].is_null(),
            "skipped step carried {key}: {steps:?}"
        );
    }
    assert_eq!(steps[3]["status"], "skipped");
}

// #47, D30. A failure carries `rc` and `stderr` whatever it ran, because the
// reason is what a runner reads -- but `stdout` is the key only a command can
// fill, so a typed action's failure has the two without the third. The values
// it used to carry were invented: `rc: 1` off the synthetic `Output` the
// overrides run on, and `rc: 0` for a `failed_when` trip, which is a failed
// step reporting that it exited successfully. `.github/check-stream.py` reads
// this same shape in the gate's contract job.
#[test]
fn json_reports_a_typed_failure_without_inventing_an_exit_code() {
    let dir = tempfile::tempdir().unwrap();
    let path = fixture("json_typed_failure.yml");
    let out = run_in(
        dir.path(),
        &["apply", path.to_str().unwrap(), "--json", "--keep-going"],
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let steps: Vec<serde_json::Value> = stdout
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).expect(l))
        .filter(|r| r["event"] == "step")
        .collect();
    assert_eq!(steps.len(), 3, "{stdout}");

    for step in &steps[1..] {
        assert_eq!(step["status"], "failed", "{stdout}");
        // The reason reaches the runner...
        assert!(step["message"].is_string(), "{stdout}");
        assert!(step.get("stderr").is_some(), "{stdout}");
        // ...with no exit code behind it, and never a fabricated one.
        assert!(step.get("rc").is_some(), "{stdout}");
        assert!(
            step["rc"].is_null(),
            "a typed failure invented an rc: {stdout}"
        );
        // The key only a command can fill stays absent.
        assert!(step.get("stdout").is_none(), "{stdout}");
    }

    assert!(
        steps[1]["message"]
            .as_str()
            .unwrap()
            .contains("not a directory"),
        "{stdout}"
    );
    assert_eq!(steps[2]["message"], "failed_when", "{stdout}");
}

// #45. The manifest counts what the run will attempt (`when` and tags
// applied); the summary counts every step the walk reached. They are
// different sets and the output used to call both "N steps", so one run
// reported two step counts. The number is right -- `when` is the one gate the
// preflight can answer up front -- so the fix is the word.
#[test]
fn the_manifest_counts_what_will_run_and_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let plan = dir.path().join("plan.yml");
    std::fs::write(
        &plan,
        "- name: a\n  shell: \"true\"\n  changed_when: \"false\"\n\
         - name: b\n  shell: \"true\"\n  when: false\n",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_provision"))
        .args(["apply", &plan.display().to_string()])
        .output()
        .unwrap();
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);

    assert!(text.contains("1 step to run"), "manifest: {text}");
    assert!(
        text.contains("2 steps · 1 ok · 1 skipped"),
        "summary: {text}"
    );
}

// D14, amended: the rule covers every template, and it used to stop at the
// boundary between a step's fields and a file's bytes. A fabricated diff is
// worse than no verdict -- it is plan claiming to know something it invented.
#[test]
fn a_j2_body_that_reads_a_register_is_unprobed_not_diffed() {
    let dir = tempfile::tempdir().unwrap();
    let path = fixture("j2_register/plan.yml");
    let res = run_in(dir.path(), &["plan", path.to_str().unwrap()]);
    let out =
        String::from_utf8_lossy(&res.stdout).into_owned() + &String::from_utf8_lossy(&res.stderr);
    // Exit 2: the static template genuinely would change.
    assert_eq!(res.status.code(), Some(2), "{out}");

    let line = |name: &str| {
        out.lines()
            .find(|l| l.contains(name))
            .unwrap_or_else(|| panic!("no line for {name}:\n{out}"))
    };
    // The derived one claims nothing and prints no diff.
    assert!(
        line("reads the register").contains("would run (unprobed)"),
        "{out}"
    );
    assert!(
        !out.contains("fingerprint:"),
        "a diff was fabricated:\n{out}"
    );
    // The static one is still judged normally -- the rule narrowed to the
    // bodies that actually read a register, not to `template` as a whole.
    assert!(line("reads nothing").contains("would change"), "{out}");
}

// Spec §4, "a register for a step that never ran". The table there is a
// promise about six field values, so it is pinned here field by field --
// including the trap it exists to name: `rc == 0` is true for a step that was
// never spawned, so the fan-in gate everybody writes passes a stage that did
// not run. Filed as #33 after a review wrote exactly that gate.
#[test]
fn a_skipped_steps_register_holds_the_documented_shape() {
    let dir = tempfile::tempdir().unwrap();
    let (code, out) = apply(dir.path(), "skipped_register.yml", &[]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("3 skipped"), "{out}");
}

// A skipped step's name is rendered like any other -- except a tag-excluded
// one, which is filtered before anything of it is rendered (spec §8) so that
// it cannot fail on a variable it was never meant to read. Rendering its name
// would put that failure straight back.
#[test]
fn a_skipped_steps_name_is_rendered_unless_tags_excluded_it() {
    let dir = tempfile::tempdir().unwrap();
    let (code, out) = apply(dir.path(), "skipped_name.yml", &["--skip-tags", "never"]);
    assert_eq!(code, 0, "{out}");

    // Skipped by `unless`, and the name says what it greets.
    assert!(
        out.contains("greets world"),
        "the name did not render:\n{out}"
    );
    assert!(!out.contains("greets {{ who }}"), "{out}");

    // Excluded by tags: raw even where rendering would have succeeded. This
    // is the assertion that catches a missing guard -- the undefined-variable
    // case below cannot, because a name that fails to render falls back to raw
    // and so reads the same either way.
    assert!(
        out.contains("excluded {{ who }}"),
        "a tag-excluded name must stay raw:\n{out}"
    );
    assert!(
        !out.contains("excluded world"),
        "a tag-excluded name rendered:\n{out}"
    );

    // And one that would not render at all is still not an error.
    assert!(
        out.contains("excluded {{ never_defined_anywhere }}"),
        "{out}"
    );
    assert!(!out.contains("undefined variable"), "{out}");
    assert!(out.contains("3 skipped"), "{out}");
}

#[test]
fn hide_skipped_drops_the_lines_but_not_the_count() {
    let dir = tempfile::tempdir().unwrap();
    let (code, out) = apply(dir.path(), "verdicts.yml", &["--hide-skipped"]);
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains("work is done"), "{out}");
    assert!(
        out.contains("1 skipped"),
        "the summary still counts it:\n{out}"
    );
    snapshot!("hide_skipped", out);
}

#[test]
fn verbose_shows_the_output_of_a_step_that_worked() {
    let dir = tempfile::tempdir().unwrap();
    let (code, quiet) = apply(dir.path(), "verdicts.yml", &[]);
    assert_eq!(code, 0, "{quiet}");
    assert!(!quiet.contains("│ hello"), "{quiet}");

    let (code, loud) = apply(dir.path(), "verdicts.yml", &["--verbose"]);
    assert_eq!(code, 0, "{loud}");
    assert!(loud.contains("│ hello"), "{loud}");
    snapshot!("verbose", loud);
}

// ── snapshots ─────────────────────────────────────────────────────────────

#[test]
fn snapshot_plain_output() {
    let dir = tempfile::tempdir().unwrap();
    let (_, out) = apply(dir.path(), "verdicts.yml", &[]);
    snapshot!("plain", out);
}

#[test]
fn snapshot_the_failure_block() {
    let dir = tempfile::tempdir().unwrap();
    let (_, out) = apply(dir.path(), "failure.yml", &[]);
    snapshot!("failure", out);
}

// Issue #4, spec §6.1 and §9.1: both streams, stdout first. The renderer used
// to pick one — stderr when it had any — so a gate that wrote its report to
// stdout and its verdict to stderr rendered as "failed on the findings above"
// with no findings above it.
#[test]
fn snapshot_a_failure_that_wrote_to_both_streams() {
    let dir = tempfile::tempdir().unwrap();
    let (_, out) = apply(dir.path(), "failure_both_streams.yml", &[]);
    snapshot!("failure_both_streams", out);
}

#[test]
fn a_failure_shows_stdout_as_well_as_stderr() {
    let dir = tempfile::tempdir().unwrap();
    let (code, out) = apply(dir.path(), "failure_both_streams.yml", &[]);
    assert_eq!(code, 1, "{out}");
    assert!(
        out.contains("finding: /etc/pacman.conf has no multilib section"),
        "the stdout report is the whole diagnostic and must show:\n{out}"
    );
    assert!(
        out.contains("gate failed on the findings above"),
        "the stderr verdict must still show:\n{out}"
    );
    // Order is fixed and is the same order `--verbose` uses on a step that
    // worked, so a reader never has to guess which stream a line came from.
    let report = out.find("finding: /etc").expect("checked above");
    let verdict = out.find("gate failed on").expect("checked above");
    assert!(report < verdict, "stdout comes first:\n{out}");
    // Both streams fit the window whole, so the note names neither and offers
    // no `--verbose`: the reader has everything, and an offer of more would be
    // a lie (§9.1). The absence of an elision marker is the announcement.
    assert!(
        !out.contains("elided"),
        "nothing was cut, so nothing says it was:\n{out}"
    );
    assert!(
        !out.contains("--verbose for all"),
        "there is nothing left to offer:\n{out}"
    );
}

// `--verbose` on a failure was the documented way to the full output, and it
// dropped stdout too: the failure block took the same one-stream path.
#[test]
fn verbose_shows_both_streams_of_a_failed_step_in_full() {
    let dir = tempfile::tempdir().unwrap();
    let (_, out) = apply(dir.path(), "failure_both_streams.yml", &["--verbose"]);
    assert!(out.contains("checked 5 things"), "{out}");
    assert!(
        out.contains("finding: /etc/pacman.conf has no multilib section"),
        "{out}"
    );
    assert!(out.contains("gate failed on the findings above"), "{out}");
    assert!(
        !out.contains("--verbose for all"),
        "there is nothing left to offer:\n{out}"
    );
}

// The other half of the slot. A broken `unless` never reaches the step's own
// command, so the event carries no streams and `Failure::stderr` is a
// synthesized reason — it still has to land in the block. This is the case
// reading `ev` instead of `f` would have silently emptied.
#[test]
fn a_failure_with_no_streams_still_shows_its_reason() {
    let dir = tempfile::tempdir().unwrap();
    let (code, out) = apply(dir.path(), "broken_gate.yml", &[]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("`unless` could not run"), "{out}");
    assert!(
        !out.contains("stdout"),
        "there was no stdout to name:\n{out}"
    );
    assert!(
        !out.contains("elided"),
        "a one-line reason is not a stream the window cut:\n{out}"
    );
}

// ── issue #59: the failure window, and what a nested `use` reports ────────

// The defect, exactly: a linter prints the rule it broke first, and a window
// that kept only the tail dropped it — so the one line the failure was about
// was invisible and the reader had to run the command again by hand.
#[test]
fn a_failing_step_shows_the_head_of_its_output_not_only_the_tail() {
    let dir = tempfile::tempdir().unwrap();
    let (code, out) = apply(dir.path(), "failure_long_output.yml", &[]);
    assert_eq!(code, 1, "{out}");
    assert!(
        out.contains("biome ▸ lint/suspicious/noExplicitAny"),
        "the rule name is the first line and must survive the window:\n{out}"
    );
    assert!(
        out.contains("15 files checked, 1 error"),
        "the tail is still shown:\n{out}"
    );
    // Both numbers, because either alone is a half-answer: how much was
    // dropped, out of how much there was.
    assert!(
        out.contains("… 25 of 55 lines elided …"),
        "the elision names what it dropped and out of what:\n{out}"
    );
    assert!(
        !out.contains("line 20 of the middle"),
        "the middle is what was cut:\n{out}"
    );
    assert!(
        out.contains("stdout, first 10 + last 20 lines · --verbose for all"),
        "the note names the stream the window narrowed and the way out:\n{out}"
    );
    snapshot!("failure_head_and_tail", out);
}

// The other side of the cap. A window that never says "I let this through
// whole" is one a reader cannot trust, so the boundary is pinned: exactly
// 10 + 20 prints whole with no marker and no offer, and one line more elides
// exactly one.
#[test]
fn the_window_fires_only_when_there_is_something_to_cut() {
    let dir = tempfile::tempdir().unwrap();
    let (code, out) = apply(dir.path(), "failure_window_boundary.yml", &["--keep-going"]);
    assert_eq!(code, 1, "{out}");
    let (at_cap, over) = out
        .split_once("One line past the window")
        .expect("both steps ran under --keep-going");
    assert!(
        !at_cap.contains("elided") && !at_cap.contains("--verbose for all"),
        "a stream that fit says nothing about a cap:\n{at_cap}"
    );
    assert!(
        over.contains("… 1 of 31 lines elided …"),
        "one line past the window elides exactly one:\n{over}"
    );
    snapshot!("failure_window_boundary", out);
}

// `--verbose` already printed every nested step and its output; what it could
// not say was how a nested block came out. Two levels, closing innermost
// first, the outer counting the inner's steps as its own.
#[test]
fn a_nested_use_closes_with_its_own_summary_under_verbose() {
    let dir = tempfile::tempdir().unwrap();
    let (quiet_code, quiet) = apply(dir.path(), "nested_use.yml", &[]);
    assert_eq!(quiet_code, 0, "{quiet}");
    assert!(
        !quiet.contains("└"),
        "a block's roll-up is detail, and a quiet run reports the run:\n{quiet}"
    );

    let (code, out) = apply(dir.path(), "nested_use.yml", &["--verbose"]);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("└ ui gate · 2 steps · 1 changed · 1 ok ·"),
        "the inner call site answers for its own two steps:\n{out}"
    );
    assert!(
        out.contains("└ Developer toolchains · 3 steps · 1 changed · 2 ok ·"),
        "the outer counts every step under it, nested ones included:\n{out}"
    );
    let inner = out.find("└ ui gate").expect("checked above");
    let outer = out.find("└ Developer").expect("checked above");
    assert!(inner < outer, "the levels close innermost first:\n{out}");
    snapshot!("nested_use_summary", out);
}

#[test]
fn snapshot_colored_failure_window_and_nested_summary() {
    // The TTY form of both new shapes: the elision marker is dim inside the
    // failure's red gutter, and a call site's summary line is dim like the
    // header it answers (spec §9.1). What a captured run can pin down is the
    // color — the spinner needs a real terminal.
    let dir = tempfile::tempdir().unwrap();
    let colored = |plan: &str, extra: &[&str]| {
        let path = fixture(plan);
        let mut args = vec!["apply", path.to_str().unwrap(), "--color", "always"];
        args.extend_from_slice(extra);
        let out = Command::new(env!("CARGO_BIN_EXE_provision"))
            .args(&args)
            .env("PROVISION_SCRATCH", dir.path())
            .env_remove("NO_COLOR")
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).replace('\x1b', "ESC")
    };
    snapshot!(
        "failure_head_and_tail_colored",
        colored("failure_long_output.yml", &[])
    );
    snapshot!(
        "nested_use_summary_colored",
        colored("nested_use.yml", &["--verbose"])
    );
}

#[test]
fn snapshot_retry_rendering() {
    let dir = tempfile::tempdir().unwrap();
    let (_, out) = apply(dir.path(), "retry.yml", &[]);
    snapshot!("retry", out);
}

// ── plan output, every verdict at once ──────────────────────────────────────
//
// plan_all_verdicts.yml carries one step per plan-mode verdict reachable from
// a plain invocation: `ok` (an assert that already holds), `would change` with
// a diff (a `file` step against the scratch dir), `would run` (a gated shell
// not yet done, and since D20 an ungated one too), and `skipped` with its
// reason (a gate that says the work is done already). The same file under
// `--plan-no-probe` collapses every line to `would run (unprobed)` (spec §7),
// which is that verdict's own case. `unknown` needs a host-dependent action
// and is asserted from its three real sources instead — see the fixture.

/// The diff under "would change" carries the scratch dir's real path
/// (`+++ /tmp/.../hello.txt`), and that path is different every run — a
/// snapshot can't carry it. Nothing else in this file's output touches the
/// filesystem: every other line is a static step name.
fn redact_scratch(text: &str, scratch: &Path) -> String {
    text.replace(&scratch.display().to_string(), "[SCRATCH]")
}

#[test]
fn snapshot_plan_text_shows_every_verdict() {
    let dir = tempfile::tempdir().unwrap();
    let path = fixture("plan_all_verdicts.yml");
    let out = run_in(dir.path(), &["plan", path.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(2));
    let text = redact_scratch(&String::from_utf8_lossy(&out.stdout), dir.path());
    for verdict in ["ok", "would change", "would run", "skipped"] {
        assert!(text.contains(verdict), "{verdict} missing:\n{text}");
    }
    snapshot!("plan_all_verdicts", text);
}

#[test]
fn snapshot_plan_no_probe_is_would_run_unprobed_throughout() {
    let dir = tempfile::tempdir().unwrap();
    let path = fixture("plan_all_verdicts.yml");
    let out = run_in(
        dir.path(),
        &["plan", "--plan-no-probe", path.to_str().unwrap()],
    );
    assert_eq!(out.status.code(), Some(0));
    let text = String::from_utf8_lossy(&out.stdout);
    // 6, not 5: the summary line names the verdict too ("5 would run
    // (unprobed)"), on top of one line per step. That accounts for every
    // occurrence in the text, so no line's own verdict column can be
    // anything else — a substring check for "would change" would also match
    // those words inside the fixture's own step name ("that would change").
    assert_eq!(text.matches("would run (unprobed)").count(), 6, "{text}");
    snapshot!("plan_all_verdicts_no_probe", text);
}

#[test]
fn plan_json_status_matches_spec_9_3_exactly() {
    let dir = tempfile::tempdir().unwrap();
    let path = fixture("plan_all_verdicts.yml");
    let out = run_in(dir.path(), &["plan", path.to_str().unwrap(), "--json"]);
    let stdout = redact_scratch(&String::from_utf8_lossy(&out.stdout), dir.path());
    let rows: Vec<serde_json::Value> = stdout
        .lines()
        .map(|l| serde_json::from_str(l).expect(l))
        .collect();

    let steps: Vec<&serde_json::Value> = rows.iter().filter(|r| r["event"] == "step").collect();
    assert_eq!(steps.len(), 5, "{stdout}");
    let statuses: Vec<&str> = steps
        .iter()
        .map(|s| s["status"].as_str().unwrap())
        .collect();
    // Spec §9.3's exact vocabulary, no others — `ok`, `would_change`,
    // `would_run`, `skipped` here; `changed`, `failed`, `unknown` and
    // `would_run_unprobed` belong to fixtures elsewhere. Two `would_run`:
    // the gated shell, and the ungated one that used to be `unknown` (D20).
    assert_eq!(
        statuses,
        vec!["ok", "would_change", "would_run", "would_run", "skipped"],
        "{stdout}"
    );
    assert!(
        steps[1]["diff"].as_str().unwrap().contains("+one"),
        "{stdout}"
    );
    assert_eq!(steps[4]["reason"], "unless", "{stdout}");

    let summary = rows.last().unwrap();
    assert_eq!(summary["event"], "summary");
    assert_eq!(summary["total"], 5, "{stdout}");

    snapshot!("plan_all_verdicts_json", stdout);
}

#[test]
fn the_json_summary_counts_a_would_change_step() {
    // Regression test for a defect this test found and reported rather than
    // patched around: `Summary` tracked `would_change` (src/output/event.rs,
    // used by the text renderer's own summary line) but src/output/json.rs
    // never serialized it, so a `plan --json` summary with a would-change
    // step was missing a key its own per-step `status` vocabulary promises
    // exists, and its component counts silently didn't add up to `total`.
    // Fixed while this batch was in flight.
    let dir = tempfile::tempdir().unwrap();
    let path = fixture("plan_all_verdicts.yml");
    let out = run_in(dir.path(), &["plan", path.to_str().unwrap(), "--json"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let summary: serde_json::Value = serde_json::from_str(stdout.lines().last().unwrap()).unwrap();
    assert_eq!(summary["would_change"], 1, "{stdout}");
    let counted = summary["ok"].as_u64().unwrap()
        + summary["changed"].as_u64().unwrap()
        + summary["would_change"].as_u64().unwrap_or(0)
        + summary["would_run"].as_u64().unwrap()
        + summary["would_run_unprobed"].as_u64().unwrap()
        + summary["unknown"].as_u64().unwrap()
        + summary["skipped"].as_u64().unwrap()
        + summary["failed"].as_u64().unwrap();
    assert_eq!(counted, summary["total"].as_u64().unwrap(), "{stdout}");
}

#[test]
fn a_when_reading_a_register_stays_unprobed_under_plan() {
    // D14 / spec §10: a `when` reading a `register` cannot be judged before
    // the step that registers it has run, so it is `would run (unprobed)`,
    // not a real verdict. This was requested as a failing test — by the time
    // it was written, e5c514f had already landed and fixed it (`condition()`
    // returned `Cond::Unprobed` correctly, but `step()` matched it together
    // with `Cond::True` and reported a real verdict anyway; a `service:
    // {state: restarted}` gated this way came out `would change`, an
    // invented answer about a machine nobody asked). This is the regression
    // test for that fix instead.
    let dir = tempfile::tempdir().unwrap();
    let path = fixture("register_unprobed.yml");
    let out = run_in(
        dir.path(),
        &["plan", path.to_str().unwrap(), "--json", "--color=never"],
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let rows: Vec<serde_json::Value> = stdout
        .lines()
        .map(|l| serde_json::from_str(l).expect(l))
        .collect();
    let steps: Vec<&serde_json::Value> = rows.iter().filter(|r| r["event"] == "step").collect();
    assert_eq!(steps.len(), 3, "{stdout}");
    assert_eq!(steps[0]["status"], "would_run", "{stdout}");
    assert_eq!(steps[1]["status"], "would_run_unprobed", "{stdout}");
    assert_eq!(steps[2]["status"], "would_run_unprobed", "{stdout}");
    let summary = rows.last().unwrap();
    assert_eq!(summary["would_run_unprobed"], 2, "{stdout}");

    // At apply time the register holds a real result and step 1 did change,
    // so both gated steps run.
    let dir = tempfile::tempdir().unwrap();
    let (code, out) = apply(dir.path(), "register_unprobed.yml", &[]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(verdict(&out, "Registers a result"), "changed", "{out}");
    assert!(
        !line_for(&out, "Gated on that register").contains("skipped"),
        "{out}"
    );
    assert!(
        !line_for(&out, "Same gate, no other modifier").contains("skipped"),
        "{out}"
    );
}

#[test]
fn a_filter_on_a_register_is_unprobed_until_apply_computes_it() {
    // Regression test for #16: `{{ crc.stdout | trim | int }}` failed
    // validate, plan and apply's pre-walk on the placeholder's empty string.
    let dir = tempfile::tempdir().unwrap();
    let path = fixture("register_derived.yml");
    let path = path.to_str().unwrap();

    let out = run_in(dir.path(), &["validate", "--strict", path]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let out = run_in(dir.path(), &["plan", path, "--json", "--color=never"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let steps: Vec<serde_json::Value> = stdout
        .lines()
        .map(|l| serde_json::from_str(l).expect(l))
        .filter(|r: &serde_json::Value| r["event"] == "step")
        .collect();
    let statuses: Vec<&str> = steps
        .iter()
        .map(|s| s["status"].as_str().unwrap())
        .collect();
    assert_eq!(
        statuses,
        [
            "would_run",
            "would_run_unprobed",
            "would_run_unprobed",
            "would_run_unprobed"
        ],
        "{stdout}"
    );

    let (code, out) = apply(dir.path(), "register_derived.yml", &[]);
    assert_eq!(code, 0, "{out}");
    let read = |f: &str| std::fs::read_to_string(dir.path().join(f)).unwrap();
    assert_eq!(read("from-field"), "1234568");
    assert_eq!(read("from-derived"), "55499");
    assert_eq!(verdict(&out, "A when reads a derived var"), "ok", "{out}");
}

#[test]
fn a_typo_beside_a_register_is_still_an_error() {
    // The tolerance is per field: a field that reads no register keeps strict
    // undefined (D6), even on a step whose other fields read one.
    let dir = tempfile::tempdir().unwrap();
    let plan = dir.path().join("typo.yml");
    std::fs::write(
        &plan,
        "- shell: \"true\"\n  changed_when: \"false\"\n  register: r\n\
         - shell: echo {{ r.stdout | int }}\n  cwd: \"{{ nope }}\"\n  changed_when: \"false\"\n",
    )
    .unwrap();
    let out = run_in(dir.path(), &["validate", plan.to_str().unwrap()]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(3), "{stderr}");
    assert!(stderr.contains("undefined variable `nope`"), "{stderr}");
}

// ── context, streaming, and Ctrl-C ────────────────────────────────────────

#[test]
fn cwd_and_env_both_reach_the_step() {
    let dir = tempfile::tempdir().unwrap();
    let (code, out) = apply(dir.path(), "context.yml", &[]);
    assert_eq!(code, 0, "{out}");
    // Written by a relative path, so it landed only if `cwd` was honored.
    assert_eq!(
        std::fs::read_to_string(dir.path().join("from-cwd")).unwrap(),
        "hello"
    );
}

#[test]
fn stream_writes_the_child_output_through_once() {
    let dir = tempfile::tempdir().unwrap();
    let (code, out) = apply(dir.path(), "stream.yml", &["--stream"]);
    assert_eq!(code, 0, "{out}");
    // Streamed *and* captured (spec §10), but only rendered once: the capture
    // is for `register`, not for a second copy on screen.
    assert_eq!(out.matches("STREAMED-MARKER").count(), 1, "{out}");
}

// Spec §10: the reader asking for less is not an error, and that holds for a
// step's own output under `--stream` too. The streamed copy went through
// `print!`, which panics on EPIPE: the drain thread died, the child took
// SIGPIPE writing into a pipe nobody read, and the step failed with its
// captured output gone. `set -o pipefail` is load-bearing, as in `list`'s
// test: without it the pipeline reports `head`'s status.
#[test]
fn stream_into_a_closed_stdout_is_not_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let script = format!(
        "set -o pipefail; '{}' apply '{}' --stream | head -c 20 >/dev/null",
        env!("CARGO_BIN_EXE_provision"),
        fixture("stream_closed_pipe.yml").display()
    );
    let out = Command::new("bash")
        .args(["-c", &script])
        .env("PROVISION_SCRATCH", dir.path())
        .env("NO_COLOR", "1")
        .output()
        .expect("bash failed to start");
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        !stderr.contains("panicked"),
        "--stream panicked when its reader went away:\n{stderr}"
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "the reader asking for less is not an error:\n{stderr}"
    );
}

// Spec §8: `--keep-going` is about a step failing, not about the operator
// stopping. Ctrl-C ends the run whatever the flag says.
#[test]
fn keep_going_does_not_carry_the_run_past_ctrl_c() {
    let dir = tempfile::tempdir().unwrap();
    let path = fixture("interrupt_then_more.yml");
    let child = Command::new(env!("CARGO_BIN_EXE_provision"))
        .args(["apply", path.to_str().unwrap(), "--keep-going"])
        .env("PROVISION_SCRATCH", dir.path())
        .env("NO_COLOR", "1")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    std::thread::sleep(std::time::Duration::from_millis(600));
    // The assertions below are about what the child did with the signal, so
    // whether `kill` itself succeeded is not what this test is measuring.
    #[expect(
        clippy::let_underscore_must_use,
        reason = "the child's own output is the assertion"
    )]
    let _ = Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status();

    let out = child.wait_with_output().unwrap();
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(130), "{text}");
    assert!(
        !text.contains("Must not run after an interrupt"),
        "kept going past Ctrl-C:\n{text}"
    );
}

// Spec §10, D19. The exit code is the least of it: before the handler existed
// provision died where it stood, so the assertions that matter are that a
// summary was printed at all and that the step's *grandchild* is gone.
//
// `orphan.txt` is the second one. The fixture backgrounds an `sh` that writes
// it two seconds in; a killed process group never lets it, and an orphaned one
// writes it well after provision has exited — which is exactly the failure a
// correct-looking exit code would otherwise hide. 143 is `128 + SIGTERM`, and
// the shell reports the same number for a process killed *without* a handler,
// so the code alone cannot tell the fix from the bug. The file can.
#[test]
fn sigterm_kills_the_whole_step_group_and_exits_143() {
    let dir = tempfile::tempdir().unwrap();
    let path = fixture("terminate.yml");
    let child = Command::new(env!("CARGO_BIN_EXE_provision"))
        .args(["apply", path.to_str().expect("fixture paths are UTF-8")])
        .env("PROVISION_SCRATCH", dir.path())
        .env("NO_COLOR", "1")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("provision failed to start");

    std::thread::sleep(std::time::Duration::from_millis(600));
    #[expect(
        clippy::let_underscore_must_use,
        reason = "the child's own output is the assertion"
    )]
    let _ = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status();

    let out = child.wait_with_output().expect("child was spawned");
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);

    assert_eq!(out.status.code(), Some(143), "{text}");
    assert!(
        text.contains("interrupted"),
        "the summary is still printed:\n{text}"
    );
    assert!(
        !text.contains("Must not run after a TERM"),
        "kept going past the TERM:\n{text}"
    );

    // Past the moment the backgrounded writer would have fired.
    std::thread::sleep(std::time::Duration::from_millis(2500));
    assert!(
        !dir.path().join("orphan.txt").exists(),
        "the step's process group outlived the run:\n{text}"
    );
}

// Spec §8: `--deadline` bounds the whole run, and a step's own `timeout` is
// clamped to what is left of it — without the clamp a one-second deadline on a
// step taking the ten-minute default is a one-second promise and a ten-minute
// run. The message names the run's clock rather than a step timeout the file
// does not contain, which is the difference between a reader finding the
// number and hunting for one nobody wrote.
#[test]
fn a_deadline_stops_the_run_and_exits_124() {
    let dir = tempfile::tempdir().unwrap();
    let (code, out) = apply(dir.path(), "deadline.yml", &["--deadline", "1s"]);
    assert_eq!(code, 124, "{out}");
    assert!(out.contains("deadline exceeded"), "{out}");
    assert!(
        !out.contains("Must not run after the deadline"),
        "the walk did not stop:\n{out}"
    );
}

// The other side of the clamp. A run where every timeout blamed the deadline
// would make `--deadline` unusable on any plan that sets its own, so a step
// whose `timeout` is the smaller number reads as the ordinary step timeout it
// is — and exits 1, not 124, because the clock is not why the run ended.
#[test]
fn a_steps_own_timeout_wins_when_it_is_the_smaller() {
    let dir = tempfile::tempdir().unwrap();
    let (code, out) = apply(
        dir.path(),
        "deadline_step_timeout.yml",
        &["--deadline", "5m"],
    );
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("timed out after"), "{out}");
    assert!(
        !out.contains("deadline exceeded"),
        "blamed the deadline for a step timeout:\n{out}"
    );
}

// The clamp has to hold across a retry, not just at the first attempt: a
// step whose gate keeps failing can burn through a short `--deadline` one
// `delay` at a time even though no single attempt ever ran long.
//
// Two separate guards defend that, and this test pins both — which it did
// not always do. The wall clock below catches a `delay` that sleeps on past
// the deadline. It does NOT catch the per-attempt recheck going missing:
// `pause` honours the deadline on its own, so with the recheck removed the
// loop still unwinds in about three seconds and a wall-clock bound stays
// green (#66). The recheck is pinned by its own words instead — see the
// `before attempt` assertion — which is exact rather than timed and cannot
// flake on a busy host.
#[test]
fn a_deadline_stops_a_retry_between_attempts() {
    let dir = tempfile::tempdir().unwrap();
    let started = std::time::Instant::now();
    let (code, out) = apply(dir.path(), "deadline_retry.yml", &["--deadline", "1s"]);
    let elapsed = started.elapsed();
    assert_eq!(code, 124, "{out}");
    assert!(out.contains("deadline exceeded"), "{out}");
    assert!(
        !out.contains("Must not run after the deadline"),
        "the walk did not stop:\n{out}"
    );
    // The per-attempt recheck, pinned by the only thing that distinguishes
    // it from `pause`: refusing to *start* attempt 2 has its own wording,
    // and nothing else in the loop produces it. Drop the recheck and the
    // attempt runs on the stale prepare-time timeout, the loop ends on a
    // plain `exit 1` at attempt 20/20, and this assertion is what notices —
    // the exit code is 124 either way and the wall clock stays inside its
    // bound (#66).
    assert!(
        out.contains("deadline exceeded before attempt 2/20"),
        "the loop started an attempt the deadline had already closed:\n{out}"
    );
    // The regression this one guards is the other half: a `delay` that is
    // not deadline-aware sleeps all 20 attempts * 1s (~20s) against a 1s
    // deadline before anything reports it. The between-step deadline check
    // alone still yields exit 124, so only wall-clock time tells those two
    // apart. It takes both defects together to reach that twenty seconds;
    // either one alone is caught by one of the two assertions here (#66).
    //
    // Ten seconds, not five. The gap being read is one second against
    // twenty, so anything between the two separates them just as well, and
    // the bound's only other job is to not go off on a busy host. Five left
    // four seconds for provision to start and run an attempt, which a loaded
    // box can eat (#64); ten leaves nine and still sits ten short of the
    // twenty a stale timeout would take. Costs nothing when the test passes:
    // a passing run ends in about a second either way.
    assert!(
        elapsed < std::time::Duration::from_secs(10),
        "the retry ran past the deadline instead of stopping between attempts: {elapsed:?}"
    );
}

// Spec §8: no step runs past the deadline it was admitted under, and a retry's
// `delay` is part of the step. It used to be one uninterruptible sleep, so a
// 30s delay against a 1s deadline ran the full 30s. And when the clock then
// stopped the loop, the step reported `deadline exceeded after 0ms` with no
// exit code — §10's "the register holds the last attempt" had nothing in it.
#[test]
fn a_deadline_reaches_into_a_retry_delay_and_keeps_the_last_attempt() {
    let dir = tempfile::tempdir().unwrap();
    let started = std::time::Instant::now();
    let (code, out) = apply(
        dir.path(),
        "retry_delay_clock.yml",
        &["--deadline", "1s", "--json"],
    );
    let elapsed = started.elapsed();
    assert_eq!(code, 124, "{out}");
    // Ten seconds, not five: the delay this must not sleep out is thirty, so
    // the bound has twenty to spare below the regression and nine above a
    // passing run, instead of the four that a loaded host could eat (#64).
    assert!(
        elapsed < std::time::Duration::from_secs(10),
        "slept out the delay past the deadline: {elapsed:?}\n{out}"
    );
    assert!(
        !out.contains("Must not run after the stop"),
        "the walk did not stop:\n{out}"
    );
    let step = out
        .lines()
        .find(|l| l.contains("Fails and waits to retry"))
        .unwrap_or_default();
    assert!(
        step.contains(r#""rc":5"#),
        "the last attempt's rc is gone:\n{out}"
    );
    assert!(
        step.contains("boom"),
        "the last attempt's stderr is gone:\n{out}"
    );
    assert!(
        !step.contains("after 0ms"),
        "a duration nobody waited:\n{out}"
    );
    // And it says which attempt it stopped before, which is how a reader
    // tells "the clock ran out while attempt 1 was running" from "the clock
    // ran out in the delay before attempt 2". Only the second is this
    // fixture, and the attempt numbers are the whole difference -- §10's
    // "the register holds the last attempt" is unreadable without them.
    assert!(
        step.contains("deadline exceeded before attempt 2/3"),
        "the stop did not name the attempt it stopped before:\n{out}"
    );
    assert!(
        step.contains(r#""attempt":1"#) && step.contains(r#""attempts":3"#),
        "§9.3's attempt keys are not on the step that retried:\n{out}"
    );
}

// The same sleep, the other stop: a TERM during a retry's `delay` ends the run
// now, not when the delay is over.
#[test]
fn sigterm_reaches_into_a_retry_delay() {
    let dir = tempfile::tempdir().unwrap();
    let path = fixture("retry_delay_clock.yml");
    let started = std::time::Instant::now();
    let child = Command::new(env!("CARGO_BIN_EXE_provision"))
        .args(["apply", path.to_str().expect("fixture paths are UTF-8")])
        .env("PROVISION_SCRATCH", dir.path())
        .env("NO_COLOR", "1")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("provision failed to start");

    // Wait for the first attempt to have run, rather than sleeping a fixed
    // 600ms and hoping. The old guess raced provision's own startup: on a
    // loaded host the TERM arrived before the handler was installed, the
    // default disposition killed the run outright, and the test failed on
    // `status.code()` being None rather than on the timing it means to
    // assert -- three of six runs on a busy box (#64). The step writes the
    // marker, so seeing it proves the walk is under way and the handler is
    // up.
    let attempted = dir.path().join("attempted");
    let waited = std::time::Instant::now();
    while !attempted.exists() {
        assert!(
            waited.elapsed() < std::time::Duration::from_secs(30),
            "the first attempt never ran"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    // And let it reach the `delay` it is meant to be interrupted inside.
    std::thread::sleep(std::time::Duration::from_millis(200));
    #[expect(
        clippy::let_underscore_must_use,
        reason = "the child's own output is the assertion"
    )]
    let _ = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status();

    let out = child.wait_with_output().expect("child was spawned");
    let elapsed = started.elapsed();
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);

    assert_eq!(out.status.code(), Some(143), "{text}");
    // Ten seconds, not five, for the same reason as the deadline half above:
    // the delay is thirty, so ten separates stopped-now from slept-it-out
    // with room on both sides that a busy host cannot close (#64).
    assert!(
        elapsed < std::time::Duration::from_secs(10),
        "slept out the delay past the TERM: {elapsed:?}\n{text}"
    );
    assert!(
        !text.contains("Must not run after the stop"),
        "kept going past the TERM:\n{text}"
    );
}

// Spec §8: `--keep-going` is about a step failing, not about the run's clock
// running out. It carries on past the first and never past the second.
#[test]
fn keep_going_does_not_carry_the_run_past_the_deadline() {
    let dir = tempfile::tempdir().unwrap();
    let (code, out) = apply(
        dir.path(),
        "deadline.yml",
        &["--deadline", "1s", "--keep-going"],
    );
    assert_eq!(code, 124, "{out}");
    assert!(
        !out.contains("Must not run after the deadline"),
        "--keep-going walked past the deadline:\n{out}"
    );
}

// Spec §8: a plan that will not render is not a plan to keep going with. The
// pre-walk catches most of that before the first step, but a `when` reading a
// register can only be evaluated once the register holds a result — and an
// error there used to be reported while the walk carried on executing.
#[test]
fn an_error_mid_apply_stops_the_walk() {
    let dir = tempfile::tempdir().unwrap();
    let (code, out) = apply(dir.path(), "when_error_stops.yml", &[]);
    assert_eq!(code, 3, "{out}");
    assert!(out.contains("not true or false"), "{out}");
    assert!(
        !dir.path().join("ran").exists(),
        "a step after the error ran:\n{out}"
    );
    // `--keep-going` is about a step failing, and this is not one.
    let (code, out) = apply(dir.path(), "when_error_stops.yml", &["--keep-going"]);
    assert_eq!(code, 3, "{out}");
    assert!(
        !dir.path().join("ran").exists(),
        "--keep-going walked past the error:\n{out}"
    );
}

// §4's grammar through §4's parser, reported before anything is walked. A
// `--deadline 5m` that meant something other than a step's `timeout: 5m` would
// be a trap laid for the one reader who noticed.
#[test]
fn a_deadline_that_is_not_a_duration_is_a_usage_error() {
    let dir = tempfile::tempdir().unwrap();
    let (code, out) = apply(dir.path(), "deadline.yml", &["--deadline", "soon"]);
    assert_eq!(code, 3, "{out}");
    assert!(out.contains("is not a duration"), "{out}");
}

// D30, the other half of D22: an interrupted step that *did* run a command
// has an exit code, and the stream used to report `null` for it while the
// step's `register` held the real one — one run, two exit codes for one step.
// The event is the source now, so `stdout` and a non-null `rc` arrive
// together here just as they do for a step that exited on its own.
#[test]
fn an_interrupted_steps_json_rc_is_the_commands_own() {
    let dir = tempfile::tempdir().unwrap();
    let path = fixture("interrupt.yml");
    let child = Command::new(env!("CARGO_BIN_EXE_provision"))
        .args([
            "apply",
            path.to_str().expect("fixture paths are UTF-8"),
            "--json",
        ])
        .env("PROVISION_SCRATCH", dir.path())
        .env("NO_COLOR", "1")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    std::thread::sleep(std::time::Duration::from_millis(600));
    #[expect(
        clippy::let_underscore_must_use,
        reason = "the child's own output is the assertion"
    )]
    let _ = Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status();
    let out = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);

    let step = stdout
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).expect(l))
        .find(|r| r["event"] == "step")
        .expect("the interrupted step is reported");
    assert_eq!(step["status"], "failed", "{stdout}");
    assert_eq!(step["interrupted"], true, "{stdout}");
    assert!(step.get("stdout").is_some(), "{stdout}");
    assert!(
        step["rc"].is_i64(),
        "a step that ran a command reported no exit code: {stdout}"
    );
}

#[test]
fn ctrl_c_kills_the_step_and_exits_130() {
    let dir = tempfile::tempdir().unwrap();
    let path = fixture("interrupt.yml");
    let child = Command::new(env!("CARGO_BIN_EXE_provision"))
        .args(["apply", path.to_str().expect("fixture paths are UTF-8")])
        .env("PROVISION_SCRATCH", dir.path())
        .env("NO_COLOR", "1")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    // Long enough for the step to be running, short enough to stay a test.
    std::thread::sleep(std::time::Duration::from_millis(600));
    // The assertions below are about what the child did with the signal, so
    // whether `kill` itself succeeded is not what this test is measuring.
    #[expect(
        clippy::let_underscore_must_use,
        reason = "the child's own output is the assertion"
    )]
    let _ = Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status();

    let out = child.wait_with_output().unwrap();
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    // Spec §10: the current step is killed, the summary is still printed, and
    // the shell's convention for SIGINT is the exit code.
    assert_eq!(out.status.code(), Some(130), "{text}");
    assert!(text.contains("interrupted"), "{text}");
    // D22's twin: the step was killed by a signal, so the terminal names the
    // code the step's `register` and the `--json` stream already report
    // rather than printing the kill with no code at all.
    assert!(
        text.contains("exit 130"),
        "an interrupted step printed no exit code:\n{text}"
    );
    assert!(
        text.contains("1 step"),
        "the summary is still printed:\n{text}"
    );
    // The step's own duration varies with exactly when the signal lands, but
    // every other line is the same fixture, the same status, and the same
    // "(interrupted)" body every time — stable enough for a snapshot once
    // durations are filtered, which `snapshot!` already does.
    snapshot!("interrupted", text);
}

#[test]
fn snapshot_colored_output() {
    // The spinner only exists on a real terminal, so what a captured run can
    // pin down is the color: which style each verdict carries (spec §9.1).
    let dir = tempfile::tempdir().unwrap();
    let path = fixture("failure.yml");
    let out = Command::new(env!("CARGO_BIN_EXE_provision"))
        .args(["apply", path.to_str().unwrap(), "--color", "always"])
        .env("PROVISION_SCRATCH", dir.path())
        .env_remove("NO_COLOR")
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).replace('\x1b', "ESC");
    snapshot!("colored", text);
}

// ── cmd, cwd, and the env sudo passes through ─────────────────────────────

#[test]
fn cmd_runs_an_argv_and_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();

    let (code, first) = apply(dir.path(), "cmd.yml", &[]);
    assert_eq!(code, 0, "{first}");
    assert!(
        line_for(&first, "spaces in it").contains("changed"),
        "{first}"
    );
    assert!(
        line_for(&first, "built in vars").contains("changed"),
        "{first}"
    );
    // No shell means no quoting: the space survives as one argument.
    assert!(dir.path().join("a dir with spaces").is_dir(), "{first}");
    assert!(dir.path().join("from-a-list").is_file(), "{first}");

    let (code, second) = apply(dir.path(), "cmd.yml", &[]);
    assert_eq!(code, 0, "{second}");
    assert!(
        !second.contains("changed"),
        "nothing should change twice:\n{second}"
    );
    assert!(second.contains("2 skipped"), "{second}");
}

#[test]
fn cwd_defaults_to_the_invocation_directory_and_can_be_overridden() {
    let dir = tempfile::tempdir().unwrap();
    let (code, out) = apply(dir.path(), "cwd_default.yml", &[]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("from-cwd-override")).unwrap(),
        "here"
    );
}

#[test]
fn an_assert_command_runs_with_the_step_env_and_cwd() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("marker"), "").unwrap();
    let (code, out) = apply(dir.path(), "assert_env_cwd.yml", &[]);
    assert_eq!(code, 1, "{out}");
    assert_eq!(
        verdict(&out, "An assert command sees the step env and cwd"),
        "ok"
    );
    assert_eq!(
        verdict(&out, "A wrong env value fails the assert"),
        "FAILED"
    );
}

#[test]
fn a_bare_plan_filename_runs_from_its_own_directory() {
    // Regression test for d531060: `Path::new("x1.yml").parent()` is
    // `Some("")`, not `None`, and `Command::current_dir("")` is ENOENT — so
    // every step in a bare-named plan died before exec, reported as
    // ``cannot run `bash`: No such file or directory``, blaming the
    // interpreter rather than the cwd. `./x1.yml` and an absolute path both
    // worked, which is what hid it: every fixture in this suite passes one
    // of those two, but the bare form is what the README documents and what
    // the justfile recipes use.
    let dir = tempfile::tempdir().unwrap();
    let plan = dir.path().join("bare_path.yml");
    std::fs::copy(fixture("bare_path.yml"), &plan).unwrap();
    let here = dir.path().canonicalize().unwrap();

    let run = |arg: &str| -> (i32, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_provision"))
            .args(["apply", arg, "--verbose"])
            .current_dir(dir.path())
            .env("NO_COLOR", "1")
            .output()
            .unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned()
                + &String::from_utf8_lossy(&out.stderr),
        )
    };

    for arg in ["bare_path.yml", "./bare_path.yml", plan.to_str().unwrap()] {
        let (code, out) = run(arg);
        assert_eq!(code, 0, "{arg}: {out}");
        assert!(
            out.contains(&here.display().to_string()),
            "{arg}: pwd was not the tempdir:\n{out}"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("out.txt")).unwrap(),
            "here\n",
            "{arg}: the relative path did not land in the plan's own directory"
        );
        std::fs::remove_file(dir.path().join("out.txt")).unwrap();
    }
}

#[test]
fn changed_when_and_friends_are_read_as_literal_booleans_too() {
    // Regression test for 3def20c: `changed_when`, `when` and `failed_when`
    // read with `as_str()`, which errors on a YAML boolean, silently
    // dropping the whole modifier. The quoted form (`changed_when: "false"`,
    // used elsewhere in this suite) worked, which is what hid it — this
    // fixture is the unquoted form everywhere.
    let dir = tempfile::tempdir().unwrap();
    let (code, out) = apply(dir.path(), "bool_modifiers.yml", &[]);
    assert_eq!(code, 0, "{out}");
    // Dropped, `changed_when: false` would report `changed` on every run
    // instead of `ok` (the step has no other gate).
    assert_eq!(verdict(&out, "changed_when false"), "ok", "{out}");
    // Dropped, `when: false` would run the step instead of skipping it.
    // ("when false" alone would also match the first step's name.)
    assert!(line_for(&out, "boolean skips").contains("skipped"), "{out}");
    assert!(!out.contains("should not run"), "{out}");
    // Dropped, `failed_when: false` would fail the step (and the whole
    // apply, at exit 1) on the `exit 1` it exists to forgive.
    assert!(!out.contains("FAILED"), "{out}");
}

#[test]
fn a_gate_that_cannot_run_fails_the_step() {
    // Spec §10: "the check is broken" and "the work is not done" are not the
    // same answer, and only one of them is safe to assume.
    let dir = tempfile::tempdir().unwrap();
    let (code, out) = apply(dir.path(), "broken_gate.yml", &[]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("`unless` could not run"), "{out}");
    assert!(out.contains("exit 127"), "{out}");
    assert!(
        !out.contains("this must never run"),
        "the step ran anyway:\n{out}"
    );
}

#[test]
fn an_unless_that_hangs_fails_the_step_instead_of_running_it() {
    // Regression test for 238e95f: a gate that hits the step's own timeout
    // used to fall through to running the step anyway, after waiting out the
    // timeout first. Spec §10 (provisional): "the check did not answer" is
    // not "the work is not done", same rule as a gate that cannot run at
    // all (`a_gate_that_cannot_run_fails_the_step`, above).
    let dir = tempfile::tempdir().unwrap();
    let began = std::time::Instant::now();
    let (code, out) = apply(dir.path(), "hung_unless.yml", &[]);
    let elapsed = began.elapsed();
    assert_eq!(code, 1, "{out}");
    assert!(
        line_for(&out, "unless gate hangs").contains("FAILED"),
        "{out}"
    );
    assert!(out.contains("`unless` timed out after 2"), "{out}");
    assert!(
        elapsed < std::time::Duration::from_mins(1),
        "the gate was never killed: {elapsed:?}"
    );
    // The part that matters: the step's own script never ran.
    assert!(
        !dir.path().join("ran").exists(),
        "the step ran despite its gate timing out"
    );

    // And the gate's own child died with the process group, the same
    // guarantee `a_typed_actions_command_is_bound_by_the_steps_timeout` and
    // `a_timeout_kills_the_children_not_only_the_shell` prove for a step's
    // own command.
    let pid: i32 = std::fs::read_to_string(dir.path().join("gate.pid"))
        .expect("the gate never ran")
        .trim()
        .parse()
        .unwrap();
    let alive = Command::new("kill")
        .args(["-0", &pid.to_string()])
        .output()
        .is_ok_and(|o| o.status.success());
    assert!(!alive, "pid {pid} outlived the gate that timed out");
}

#[test]
fn plan_reports_a_hung_unless_as_failed_and_keeps_walking() {
    let dir = tempfile::tempdir().unwrap();
    let path = fixture("hung_unless.yml");
    let out = run_in(dir.path(), &["plan", path.to_str().unwrap()]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "{text}");
    assert!(
        line_for(&text, "unless gate hangs").contains("FAILED"),
        "{text}"
    );
    assert!(text.contains("`unless` timed out after 2"), "{text}");
    assert!(
        text.contains("1 step"),
        "the summary is still printed:\n{text}"
    );
}

#[test]
fn sudo_passes_the_steps_env_and_nothing_else() {
    if !root_is_reachable() {
        eprintln!("skipped: needs a warm `sudo -n`");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (code, out) = apply(dir.path(), "sudo_env.yml", &[]);
    // The step asserts both halves itself: GREETING arrived, and
    // PROVISION_SCRATCH — set for provision, never declared by the step — did
    // not. --preserve-env is given the step's own keys and no others.
    assert_eq!(code, 0, "{out}");
}

fn root_is_reachable() -> bool {
    Command::new("sudo")
        .args(["-n", "--", "true"])
        .output()
        .is_ok_and(|o| o.status.success())
}

// ── file ──────────────────────────────────────────────────────────────────

#[test]
fn every_file_state_applies_twice_changed_then_ok() {
    let dir = tempfile::tempdir().unwrap();

    let (code, first) = apply(dir.path(), "file_states.yml", &[]);
    assert_eq!(code, 0, "{first}");
    for name in [
        "directory with an explicit mode",
        "inline content",
        "copied from src",
        "symlink",
    ] {
        assert!(
            line_for(&first, name).contains("changed"),
            "{name}:\n{first}"
        );
    }
    // Spec §6.3: `absent` on a path that was never there is ok, not changed.
    // This is the case the second run of an idempotency test lands on, and
    // here it is already true on the first.
    assert_eq!(verdict(&first, "never there"), "ok", "{first}");

    let conf = dir.path().join("conf");
    assert_eq!(mode_of(&conf), 0o700, "the explicit mode was not applied");
    assert_eq!(
        std::fs::read_to_string(conf.join("hello.txt")).unwrap(),
        "one\ntwo\n"
    );
    assert_eq!(
        std::fs::read_to_string(conf.join("copied")).unwrap(),
        "copied from src\n"
    );
    // Stored exactly as written: a relative link stays relative.
    assert_eq!(
        std::fs::read_link(conf.join("link"))
            .unwrap()
            .to_str()
            .unwrap(),
        "./hello.txt"
    );
    // But `~` is a path field the tool owns (spec §3), so it expands. Without
    // this the link never matches what is on disk and the step reports
    // changed on every run — which is how this was found, on a real plan.
    let home = std::env::var("HOME").unwrap();
    assert_eq!(
        std::fs::read_link(conf.join("home-link"))
            .unwrap()
            .to_str()
            .unwrap(),
        format!("{home}/.bashrc")
    );

    let (code, second) = apply(dir.path(), "file_states.yml", &[]);
    assert_eq!(code, 0, "{second}");
    assert!(
        !second.contains("changed"),
        "nothing should change twice:\n{second}"
    );
    assert!(second.contains("6 ok"), "{second}");
}

/// One build-then-copy plan, written into `dir`. The `src` is relative to the
/// plan file (spec §3) and the command writes it there, which is the shape of
/// every `install.yml` in the fleet.
fn build_then_copy_plan(dir: &Path, builds: &str) -> PathBuf {
    let at = dir.display();
    let path = dir.join("install.yml");
    // Written out in full rather than escaped onto one line: the shape of the
    // plan is what this is testing.
    let body = format!(
        r#"---
- name: build the artefact
  shell: printf built > {at}/{builds}
  changed_when: "false"
- name: copy the artefact into place
  file:
    path: {at}/installed
    src: ./artefact
    state: file
    mode: "0755"
"#
    );
    std::fs::write(&path, body).expect("writing a plan into the test's own tempdir");
    path
}

#[test]
fn a_src_the_plan_builds_is_judged_when_the_step_runs_not_before() {
    // Issue #3, D18. On a clean checkout the source does not exist and cannot,
    // because the step that creates it has not run — and validation walks the
    // whole plan before any of it. The plan is not wrong; "missing" was being
    // decided too early.
    let dir = tempfile::tempdir().unwrap();
    let plan = build_then_copy_plan(dir.path(), "artefact");
    let arg = plan.to_str().expect("tempdir paths are UTF-8");

    let out = run_in(dir.path(), &["validate", arg]);
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{text}");

    // And the copy is a real `file` step either side of it: changed, then ok.
    // That reporting is the whole reason to write one rather than a `cp`.
    let out = run_in(dir.path(), &["apply", arg]);
    let first =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{first}");
    assert_eq!(verdict(&first, "copy the artefact"), "changed", "{first}");

    let installed = dir.path().join("installed");
    assert_eq!(std::fs::read_to_string(&installed).unwrap(), "built");
    assert_eq!(mode_of(&installed), 0o755);

    let out = run_in(dir.path(), &["apply", arg]);
    let second =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{second}");
    assert_eq!(verdict(&second, "copy the artefact"), "ok", "{second}");
}

#[test]
fn a_src_the_command_did_not_build_still_fails_when_the_step_is_reached() {
    // The other half of D18: deferring the check is not dropping it. The
    // command runs, writes something else, and the copy is as wrong as it
    // always was — reported at the `src` that named it, before it writes.
    let dir = tempfile::tempdir().unwrap();
    let plan = build_then_copy_plan(dir.path(), "something-else");
    let arg = plan.to_str().expect("tempdir paths are UTF-8");

    let out = run_in(dir.path(), &["apply", arg]);
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(3), "{text}");
    assert!(text.contains("file src not found"), "{text}");
    assert!(!dir.path().join("installed").exists(), "{text}");
}

#[test]
fn a_mode_that_drifts_is_brought_back() {
    let dir = tempfile::tempdir().unwrap();
    apply(dir.path(), "file_states.yml", &[]);

    let conf = dir.path().join("conf");
    set_mode(&conf, 0o755);

    let (code, out) = apply(dir.path(), "file_states.yml", &[]);
    assert_eq!(code, 0, "{out}");
    let line = line_for(&out, "directory with an explicit mode");
    assert!(line.contains("changed"), "{out}");
    assert!(
        out.contains("mode 0755 → 0700"),
        "the delta is not reported:\n{out}"
    );
    assert_eq!(mode_of(&conf), 0o700);
}

#[test]
fn plan_shows_a_diff_and_no_diff_suppresses_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = fixture("file_states.yml");

    let out = run_in(dir.path(), &["plan", path.to_str().unwrap()]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(2), "{text}");
    assert!(
        line_for(&text, "inline content").contains("would change"),
        "{text}"
    );
    assert!(text.contains("+one"), "the diff is missing:\n{text}");
    assert!(!dir.path().join("conf").exists(), "plan created something");

    let out = run_in(dir.path(), &["plan", path.to_str().unwrap(), "--no-diff"]);
    let quiet = String::from_utf8_lossy(&out.stdout);
    assert!(quiet.contains("would change"), "{quiet}");
    assert!(
        !quiet.contains("+one"),
        "--no-diff did not suppress it:\n{quiet}"
    );
}

#[test]
fn a_file_that_cannot_be_read_says_sudo_is_how() {
    // Spec §10: guessing "it must differ" would rewrite a file nobody could
    // compare. The step fails and names the way to read it.
    if root_is_reachable() {
        let dir = tempfile::tempdir().unwrap();
        let secret = dir.path().join("secret");
        std::fs::write(&secret, "x").unwrap();
        set_mode(&secret, 0o000);
        // Owner only. `root:root` names a group that does not exist on macOS,
        // where root's group is `wheel`, and the group is not what this test
        // is about — an unreadable file is unreadable by its mode.
        assert!(
            Command::new("sudo")
                .args(["-n", "chown", "root", secret.to_str().unwrap()])
                .status()
                .is_ok_and(|s| s.success())
        );

        let plan = dir.path().join("unreadable.yml");
        std::fs::write(
            &plan,
            format!(
                "- name: Rewrite a file nobody can read
  file:
    path: {}
    state: file
    content: \"y\"
",
                secret.display()
            ),
        )
        .unwrap();
        let out = run_in(dir.path(), &["apply", plan.to_str().unwrap()]);
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        #[expect(
            clippy::let_underscore_must_use,
            reason = "best-effort cleanup of a root-owned fixture"
        )]
        let _ = Command::new("sudo")
            .args(["-n", "rm", "-f", secret.to_str().unwrap()])
            .status();

        assert_eq!(out.status.code(), Some(1), "{text}");
        assert!(text.contains("sudo: true"), "{text}");
    } else {
        eprintln!("skipped: needs a warm `sudo -n`");
    }
}

#[test]
fn the_sudo_write_path_stages_outside_the_destination() {
    // The destination directory is root-owned and unwritable by the user,
    // which is exactly why a temp file "next to dest" cannot work.
    if !root_is_reachable() {
        eprintln!("skipped: needs a warm `sudo -n`");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root_dir = dir.path().join("etc");
    std::fs::create_dir(&root_dir).unwrap();
    let owned = root_dir.to_str().unwrap().to_string();
    // Owner only — `root:root` names a group macOS does not have, and the
    // mode below is what makes the directory unwritable either way.
    assert!(sudo(&["chown", "root", &owned]) && sudo(&["chmod", "0755", &owned]));

    let run = |extra: &[&str]| {
        let path = fixture("file_sudo.yml");
        let mut args = vec!["apply", path.to_str().expect("fixture paths are UTF-8")];
        args.extend_from_slice(extra);
        let out = Command::new(env!("CARGO_BIN_EXE_provision"))
            .args(&args)
            .env("PROVISION_SCRATCH", dir.path())
            .env("PROVISION_ROOT_DIR", &owned)
            // The fixture asks for a group by name, which is the `-g` path
            // worth covering — but the name is not the same everywhere.
            .env("PROVISION_ROOT_GROUP", root_group())
            .env("NO_COLOR", "1")
            .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")))
            .output()
            .unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
        )
    };

    let (code, first) = run(&[]);
    let (code2, second) = run(&[]);
    // `stat -c` is GNU's; BSD's is `-f`. The directory is traversable, so
    // reading the metadata needs no sudo even though the file itself is 0440
    // and root-owned.
    let stat_args: &[&str] = if cfg!(target_os = "macos") {
        &["-f", "%Lp %Su %Sg"]
    } else {
        &["-c", "%a %U %G"]
    };
    let stat = Command::new("stat")
        .args(stat_args)
        .arg(format!("{owned}/provision.conf"))
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    let _ = sudo(&["rm", "-rf", &owned]);

    assert_eq!(code, 0, "{first}");
    assert!(
        line_for(&first, "root-owned config").contains("changed"),
        "{first}"
    );
    assert_eq!(
        stat,
        format!("440 root {}", root_group()),
        "install did not land the metadata"
    );
    assert_eq!(code2, 0, "{second}");
    assert!(
        !second.contains("changed"),
        "the sudo path is not idempotent:\n{second}"
    );
}

#[test]
fn the_sudo_write_path_leaves_an_existing_parent_mode_alone() {
    // Spec §6.3: "A parent that already exists is not touched." `install -d
    // -m 0755` on the way to the target used to widen it, every apply.
    if !root_is_reachable() {
        eprintln!("skipped: needs a warm `sudo -n`");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let locked = dir.path().join("locked");
    std::fs::create_dir(&locked).unwrap();
    set_mode(&locked, 0o700);

    let (code, first) = apply(dir.path(), "file_sudo_parent.yml", &[]);
    let (code2, second) = apply(dir.path(), "file_sudo_parent.yml", &[]);
    let after = mode_of(&locked);
    let _ = sudo(&["rm", "-rf", locked.to_str().unwrap()]);

    assert_eq!(code, 0, "{first}");
    assert_eq!(code2, 0, "{second}");
    assert_eq!(after, 0o700, "the existing parent was re-moded");
}

fn sudo(args: &[&str]) -> bool {
    let mut all = vec!["-n"];
    all.extend_from_slice(args);
    Command::new("sudo")
        .args(&all)
        .status()
        .is_ok_and(|s| s.success())
}

fn mode_of(p: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p)
        .expect("the test just created this path")
        .permissions()
        .mode()
        & 0o7777
}

fn set_mode(p: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode))
        .expect("the test just created this path");
}

#[test]
fn file_creates_missing_parent_directories() {
    // Issue #11: `file` failed with "cannot make a temp file in …: No such
    // file or directory" where `template` into the same missing directory
    // succeeded. §6.3: every state that creates something creates the
    // directories leading to it, at 0755.
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("hooks-pre-commit");
    std::fs::write(&src, "#!/bin/sh\n").unwrap();
    let plan = dir.path().join("parents.yml");
    std::fs::write(
        &plan,
        format!(
            "- name: A file under a directory nobody made\n  \
             file:\n    path: \"{{{{ env.PROVISION_SCRATCH }}}}/cfg/git/hooks/pre-commit\"\n    \
             src: {}\n\
             - name: A link under another one\n  \
             file:\n    path: \"{{{{ env.PROVISION_SCRATCH }}}}/cfg/nvim/init.lua\"\n    \
             state: link\n    src: \"{{{{ env.PROVISION_SCRATCH }}}}/hooks-pre-commit\"\n\
             - name: Content under a third\n  \
             file:\n    path: \"{{{{ env.PROVISION_SCRATCH }}}}/cfg/deep/a/b/note\"\n    \
             content: \"hi\\n\"\n    mode: \"0600\"\n",
            src.display()
        ),
    )
    .unwrap();

    let out = run_in(dir.path(), &["apply", plan.to_str().unwrap()]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("cfg/git/hooks/pre-commit")).unwrap(),
        "#!/bin/sh\n"
    );
    assert!(
        std::fs::symlink_metadata(dir.path().join("cfg/nvim/init.lua"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    // The leaf's mode is its own; the directories on the way are 0755
    // whatever the umask, as §6.3 says and `state: dir` already did.
    assert_eq!(mode_of(&dir.path().join("cfg/deep/a")), 0o755);
    assert_eq!(mode_of(&dir.path().join("cfg/deep/a/b")), 0o755);
    assert_eq!(mode_of(&dir.path().join("cfg/deep/a/b/note")), 0o600);

    // §6: the second run changes nothing.
    let again = run_in(dir.path(), &["apply", plan.to_str().unwrap()]);
    let text = String::from_utf8_lossy(&again.stdout);
    assert_eq!(again.status.code(), Some(0), "{text}");
    assert!(!text.contains("changed"), "{text}");
}

#[test]
fn tilde_follows_the_scopes_home_for_a_path_on_the_target() {
    // Issue #10: a tree rendered on one machine for another baked the
    // renderer's home into every `~`. §3: a path naming a location on the
    // machine being converged, and the `expanduser` filter, expand against
    // the scope's `home`; a path read out of the plan tree does not.
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("phone-home");
    let src = dir.path().join("hook.txt");
    std::fs::write(&src, "#!/bin/sh\n").unwrap();
    let plan = dir.path().join("staged.yml");
    std::fs::write(
        &plan,
        format!(
            "- vars:\n    home: {home}\n\
             - name: A file at the target's home\n  \
             file:\n    path: ~/.config/git/config\n    content: \"[core]\\n\"\n\
             - name: A link at the target's home\n  \
             file:\n    path: ~/.config/git/hook\n    state: link\n    src: ~/.local/hook\n\
             - name: A file copied out of the plan tree\n  \
             file:\n    path: ~/.config/git/copied\n    src: ./hook.txt\n\
             - name: The filter says the same thing\n  \
             file:\n    path: ~/.config/git/rendered\n    \
             content: \"hooksPath = {{{{ '~/.config/git/hooks' | expanduser }}}}\\n\"\n",
            home = target.display()
        ),
    )
    .unwrap();

    let out = run_in(dir.path(), &["apply", plan.to_str().unwrap()]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );

    // The destinations went to the declared home, not this process's.
    assert!(target.join(".config/git/config").is_file(), "no config");
    // `src` is read out of the plan tree, on this machine, as always — the
    // copy proves it resolved here rather than under the target's home.
    assert_eq!(
        std::fs::read_to_string(target.join(".config/git/copied")).unwrap(),
        "#!/bin/sh\n"
    );
    // The link target is on the target machine too, so it reads as the
    // target's home — not as a path into this one.
    let link = std::fs::read_link(target.join(".config/git/hook")).unwrap();
    assert_eq!(link, target.join(".local/hook"));
    // And the filter, which is the half the issue was filed about.
    assert_eq!(
        std::fs::read_to_string(target.join(".config/git/rendered")).unwrap(),
        format!("hooksPath = {}/.config/git/hooks\n", target.display())
    );
}

#[test]
fn tilde_is_this_machines_home_when_nothing_shadows_it() {
    // `home` is a fact, so the rule above is invisible on an ordinary run:
    // both homes are the same one. A plan that sets nothing gets exactly
    // what it got before.
    let dir = tempfile::tempdir().unwrap();
    let plan = dir.path().join("plain.yml");
    std::fs::write(
        &plan,
        "- name: Where does it point\n  file:\n    path: \"{{ env.PROVISION_SCRATCH }}/where\"\n    content: \"{{ '~/x' | expanduser }}\\n\"\n",
    )
    .unwrap();
    let out = run_in(dir.path(), &["apply", plan.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(0));
    let got = std::fs::read_to_string(dir.path().join("where")).unwrap();
    let home = std::env::var("HOME").expect("a unix test runner has HOME");
    assert_eq!(got, format!("{home}/x\n"));
}

#[test]
fn parent_directories_are_deterministic_not_umask_derived() {
    // create_dir_all would give every level a umask-derived mode. §6.3 says
    // `mode` lands on the leaf and parents get 0755, which is what GNU
    // `install -d -m` does under any umask.
    let dir = tempfile::tempdir().unwrap();
    let plan = dir.path().join("nested.yml");
    std::fs::write(
        &plan,
        "- name: A directory three levels down\n  file:\n    path: \"{{ env.PROVISION_SCRATCH }}/a/b/c\"\n    state: dir\n    mode: \"0700\"\n",
    )
    .unwrap();

    let out = run_in(dir.path(), &["apply", plan.to_str().unwrap()]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert_eq!(
        mode_of(&dir.path().join("a")),
        0o755,
        "parent took the umask"
    );
    assert_eq!(
        mode_of(&dir.path().join("a/b")),
        0o755,
        "parent took the umask"
    );
    assert_eq!(
        mode_of(&dir.path().join("a/b/c")),
        0o700,
        "the mode missed the leaf"
    );
}

#[test]
fn an_unquoted_mode_says_to_quote_it() {
    // `mode: 0644` is octal 420 in YAML 1.1 and decimal 644 in YAML 1.2.
    // Neither is what was meant, so provision refuses rather than picking.
    let dir = tempfile::tempdir().unwrap();
    let plan = dir.path().join("unquoted.yml");
    std::fs::write(
        &plan,
        "- name: An unquoted mode\n  file:\n    path: \"{{ env.PROVISION_SCRATCH }}/f\"\n    state: file\n    content: x\n    mode: 0644\n",
    )
    .unwrap();

    let out = run_in(dir.path(), &["apply", plan.to_str().unwrap()]);
    let text = String::from_utf8_lossy(&out.stderr).into_owned();
    assert_eq!(out.status.code(), Some(3), "{text}");
    assert!(text.contains("`mode` must be a quoted string"), "{text}");
    assert!(
        text.contains("\"0644\""),
        "the note does not show the fix: {text}"
    );
    assert!(text.contains("unquoted.yml:6:11"), "{text}");
}

// ── template ──────────────────────────────────────────────────────────────

#[test]
fn a_template_and_a_tree_apply_twice_changed_then_ok() {
    let dir = tempfile::tempdir().unwrap();

    let (code, first) = apply(dir.path(), "template.yml", &[]);
    assert_eq!(code, 0, "{first}");
    assert!(
        line_for(&first, "single template").contains("changed"),
        "{first}"
    );
    // Spec §6.4: one step, one line, and the line carries the count.
    assert!(line_for(&first, "whole tree").contains("3 of 3"), "{first}");

    let out = dir.path().join("rendered");
    // The fixture interpolates `{{ os }}`, which is the point — a fact reaches
    // a template. Asserting the rendered value against a literal `linux` made
    // the test pass on the machine it was written on and nowhere else.
    assert_eq!(
        std::fs::read_to_string(dir.path().join("one")).unwrap(),
        format!("single file for {}\n", os_fact())
    );
    // The `.j2` comes off the destination name; a file without one is still
    // rendered.
    assert!(
        out.join("top.conf").is_file(),
        "the .j2 suffix was not stripped"
    );
    assert!(
        std::fs::read_to_string(out.join("nested/inner.conf"))
            .unwrap()
            .contains(os_fact())
    );
    // Spec §6.4: a file that is not valid UTF-8 is placed byte for byte.
    let src =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/apply/tree/nested/blob.bin");
    assert_eq!(
        std::fs::read(out.join("nested/blob.bin")).unwrap(),
        std::fs::read(src).unwrap(),
        "the binary file was rendered instead of copied"
    );
    assert_eq!(mode_of(&dir.path().join("one")), 0o640);
    assert_eq!(mode_of(&out.join("top.conf")), 0o644);
    // Directories made on the way are 0755, not the mode of the files in them.
    assert_eq!(mode_of(&out.join("nested")), 0o755);

    let (code, second) = apply(dir.path(), "template.yml", &[]);
    assert_eq!(code, 0, "{second}");
    assert!(
        !second.contains("changed"),
        "nothing should change twice:\n{second}"
    );
}

#[test]
fn a_tree_reports_only_the_files_that_changed() {
    let dir = tempfile::tempdir().unwrap();
    apply(dir.path(), "template.yml", &[]);
    std::fs::write(dir.path().join("rendered/top.conf"), "drifted\n").unwrap();

    let (code, out) = apply(dir.path(), "template.yml", &[]);
    assert_eq!(code, 0, "{out}");
    assert!(line_for(&out, "whole tree").contains("1 of 3"), "{out}");
    // Spec §6.4: each diff is headed by the path relative to the tree root.
    assert!(out.contains("+++ top.conf"), "{out}");
    assert!(
        !out.contains("inner.conf"),
        "an unchanged file was reported:\n{out}"
    );
}

#[test]
fn a_regular_file_where_the_tree_should_go_is_a_failure() {
    // Spec §6.4: rendering a tree over one file would place the last file and
    // silently drop the rest.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("rendered"), "in the way").unwrap();

    let (code, out) = apply(dir.path(), "template.yml", &[]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("is a regular file, not a directory"), "{out}");
}

#[test]
fn a_symlink_in_the_source_tree_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir(&src).unwrap();
    std::fs::write(src.join("real.conf"), "hello\n").unwrap();
    std::os::unix::fs::symlink("real.conf", src.join("aliased.conf")).unwrap();

    let plan = dir.path().join("linked.yml");
    std::fs::write(
        &plan,
        format!(
            "- name: A tree with a symlink in it\n  template:\n    src: {}\n    dest: {}/out\n",
            src.display(),
            dir.path().display()
        ),
    )
    .unwrap();

    let out = run_in(dir.path(), &["apply", plan.to_str().unwrap()]);
    let text = String::from_utf8_lossy(&out.stderr).into_owned();
    assert_eq!(out.status.code(), Some(3), "{text}");
    assert!(text.contains("is a symlink"), "{text}");
    assert!(text.contains("does not follow symlinks"), "{text}");
}

// ── service ───────────────────────────────────────────────────────────────

/// A throwaway systemd user unit in the **runtime** unit directory.
///
/// Never `~/.config/systemd/user`: that holds real units, and a test killed
/// between writing and cleaning up would leave a stray one there forever.
/// Runtime units live in the session and vanish with it, so the worst case
/// cleans itself up.
struct Unit {
    name: String,
    path: PathBuf,
}

impl Unit {
    fn new() -> Option<Unit> {
        // Tests share one process, so the pid alone is not unique enough:
        // two of them would build the same unit name and each Drop would
        // remove the other's file.
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

        let runtime = std::env::var("XDG_RUNTIME_DIR").ok()?;
        let dir = Path::new(&runtime).join("systemd/user");
        std::fs::create_dir_all(&dir).ok()?;
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let name = format!("provision-test-{}-{n}.service", std::process::id());
        let path = dir.join(&name);
        std::fs::write(
            &path,
            "[Unit]\nDescription=provision test unit\n\n\
             [Service]\nType=oneshot\nRemainAfterExit=yes\nExecStart=/bin/true\n\n\
             [Install]\nWantedBy=default.target\n",
        )
        .ok()?;
        if !systemctl(&["daemon-reload"]) {
            #[expect(
                clippy::let_underscore_must_use,
                reason = "unwinding a unit that was never accepted"
            )]
            let _ = std::fs::remove_file(&path);
            return None;
        }
        Some(Unit { name, path })
    }
}

impl Drop for Unit {
    // Teardown: each step is best-effort, and a failure here must not mask
    // the test's own result.
    #[expect(clippy::let_underscore_must_use, reason = "best-effort teardown")]
    fn drop(&mut self) {
        let _ = systemctl(&["stop", &self.name]);
        let _ = std::fs::remove_file(&self.path);
        let _ = systemctl(&["daemon-reload"]);
    }
}

fn systemctl(args: &[&str]) -> bool {
    let mut all = vec!["--user"];
    all.extend_from_slice(args);
    Command::new("systemctl")
        .args(&all)
        .output()
        .is_ok_and(|o| o.status.success())
}

fn service_plan(dir: &Path, name: &str, body: &str) -> PathBuf {
    let plan = dir.join("service.yml");
    std::fs::write(
        &plan,
        format!("- name: The test unit\n  service:\n    name: {name}\n    scope: user\n{body}"),
    )
    .expect("writing a plan into the test's own tempdir");
    plan
}

#[test]
fn a_user_unit_starts_stops_and_restarts_idempotently() {
    let Some(unit) = Unit::new() else {
        eprintln!("skipped: no systemd user manager reachable");
        return;
    };
    let dir = tempfile::tempdir().unwrap();

    let run = |body: &str| {
        let plan = service_plan(dir.path(), &unit.name, body);
        let out = run_in(dir.path(), &["apply", plan.to_str().unwrap()]);
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
        )
    };

    let (code, first) = run("    state: started\n");
    assert_eq!(code, 0, "{first}");
    assert!(line_for(&first, "test unit").contains("changed"), "{first}");
    assert!(first.contains(&format!("start {}", unit.name)), "{first}");

    let (code, second) = run("    state: started\n");
    assert_eq!(code, 0, "{second}");
    assert_eq!(
        verdict(&second, "test unit"),
        "ok",
        "already started:\n{second}"
    );

    // Spec §6.6: `restarted` has no before-state, so it is always changed.
    let (code, restarted) = run("    state: restarted\n");
    assert_eq!(code, 0, "{restarted}");
    assert!(
        line_for(&restarted, "test unit").contains("changed"),
        "{restarted}"
    );

    let (code, stopped) = run("    state: stopped\n");
    assert_eq!(code, 0, "{stopped}");
    assert!(
        line_for(&stopped, "test unit").contains("changed"),
        "{stopped}"
    );
    let (code, again) = run("    state: stopped\n");
    assert_eq!(code, 0, "{again}");
    assert_eq!(
        verdict(&again, "test unit"),
        "ok",
        "already stopped:\n{again}"
    );
}

#[test]
fn enabled_is_probed_without_being_set() {
    // `systemctl --user enable` writes its symlink into ~/.config/systemd/user,
    // which no test of ours may touch, so the *setting* of `enabled` is
    // manual (plan.md says so). The probe is read-only and tested here.
    let Some(unit) = Unit::new() else {
        eprintln!("skipped: no systemd user manager reachable");
        return;
    };
    let dir = tempfile::tempdir().unwrap();

    let plan = service_plan(dir.path(), &unit.name, "    enabled: true\n");
    let out = run_in(dir.path(), &["plan", plan.to_str().unwrap()]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(2), "{text}");
    assert!(text.contains(&format!("enable {}", unit.name)), "{text}");

    let plan = service_plan(dir.path(), &unit.name, "    enabled: false\n");
    let out = run_in(dir.path(), &["plan", plan.to_str().unwrap()]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(0),
        "a disabled unit is already so:\n{text}"
    );
}

#[test]
fn a_user_scope_service_may_not_ask_for_sudo() {
    // Spec §6.6: a user unit belongs to the invoking user's manager, and root
    // has a different one. The two together mean opposite things.
    let dir = tempfile::tempdir().unwrap();
    let plan = dir.path().join("bad.yml");
    std::fs::write(
        &plan,
        "- name: Both at once\n  service:\n    name: x.service\n    state: started\n    scope: user\n  sudo: true\n",
    )
    .unwrap();
    let out = run_in(dir.path(), &["apply", plan.to_str().unwrap()]);
    let text = String::from_utf8_lossy(&out.stderr).into_owned();
    assert_eq!(out.status.code(), Some(3), "{text}");
    assert!(text.contains("mean opposite things"), "{text}");
}

#[test]
fn a_service_that_declares_nothing_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let plan = dir.path().join("empty.yml");
    std::fs::write(&plan, "- name: Nothing\n  service:\n    name: x.service\n").unwrap();
    let out = run_in(dir.path(), &["apply", plan.to_str().unwrap()]);
    let text = String::from_utf8_lossy(&out.stderr).into_owned();
    assert_eq!(out.status.code(), Some(3), "{text}");
    assert!(text.contains("needs `state` or `enabled`"), "{text}");
}

// ── pkg ───────────────────────────────────────────────────────────────────

/// The host-side `pkg` tests read this machine's dpkg database and never
/// write to it. There is nothing to read on a box without apt.
fn apt_is_local() -> bool {
    let on_path = |bin: &str| {
        Command::new(bin)
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
    };
    on_path("apt-get") && on_path("dpkg-query")
}

/// One plan, written into `dir` and run through `plan`.
///
/// Validation happens before anything runs, so `plan` reaches every error
/// below and no malformed body can act.
fn pkg_plan(dir: &Path, body: &str) -> (i32, String) {
    let path = dir.join("pkg.yml");
    std::fs::write(&path, body).expect("writing a plan into the test's own tempdir");
    let out = run_in(
        dir,
        &["plan", path.to_str().expect("fixture paths are UTF-8")],
    );
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
    )
}

#[test]
fn pkg_leaves_a_host_that_already_matches_alone() {
    // Both verdicts come out of the query alone: `present` on a package the
    // database lists, and `absent` on one it does not. Neither runs the
    // manager, which is why this fixture is safe under `apply`.
    if !apt_is_local() {
        eprintln!("skipped: needs apt");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (code, out) = apply(dir.path(), "pkg_noop.yml", &[]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(verdict(&out, "base system always has"), "ok", "{out}");
    assert_eq!(verdict(&out, "An absent package"), "ok", "{out}");
    assert!(
        !out.contains("changed"),
        "nothing may change on this host:\n{out}"
    );
}

#[test]
fn pkg_plan_names_what_it_would_install_and_admits_what_it_cannot_know() {
    if !apt_is_local() {
        eprintln!("skipped: needs apt");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = fixture("pkg_plan.yml");
    let out = run_in(dir.path(), &["plan", path.to_str().unwrap()]);
    let text = String::from_utf8_lossy(&out.stdout);

    // Spec §6.5: plan lists the packages that would be installed, by actual
    // query — the name is in the detail, not just the count.
    assert!(
        line_for(&text, "not installed").contains("would change"),
        "{text}"
    );
    assert!(text.contains("install provision-no-such-package"), "{text}");
    // Spec §6.5: whether a newer version exists is not a question the local
    // database answers, so `latest` on an installed package says so.
    assert_eq!(verdict(&text, "asked for latest"), "unknown", "{text}");
    // D15: an unknown verdict is not a converged one.
    assert_eq!(out.status.code(), Some(2), "{text}");
}

#[test]
fn the_managers_that_refuse_root_refuse_sudo() {
    // Spec §6.5: brew and yay both refuse to run as root, so `sudo: true` on
    // either is a validation error rather than a failure at the far end.
    let dir = tempfile::tempdir().unwrap();
    for manager in ["brew", "yay"] {
        let (code, out) = pkg_plan(
            dir.path(),
            &format!(
                "- name: As root\n  pkg:\n    name: git\n    manager: {manager}\n  sudo: true\n"
            ),
        );
        assert_eq!(code, 3, "{out}");
        assert!(
            out.contains(&format!("`{manager}` must not run as root")),
            "{out}"
        );
    }
}

/// `provision <verb>` with PATH set to exactly `bin_dir` — nothing of the
/// host's own. The default-manager rule walks PATH (§6.5), so a test about
/// *which* backend gets resolved is a coin flip on a machine that has a real
/// one: apt wins on Debian, pacman on Arch, brew on a Mac. This is the only
/// way to ask the question and get the same answer everywhere.
fn pkg_run_with_only(dir: &Path, verb: &str, body: &str, bin_dir: &Path) -> (i32, String) {
    let path = dir.join("pkg.yml");
    std::fs::write(&path, body).expect("writing a plan into the test's own tempdir");
    let out = Command::new(env!("CARGO_BIN_EXE_provision"))
        .args([verb, path.to_str().expect("fixture paths are UTF-8")])
        .env("PATH", bin_dir.display().to_string())
        .env("NO_COLOR", "1")
        .output()
        .expect("provision failed to start");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
    )
}

#[test]
fn an_unnamed_manager_is_not_judged_by_the_validating_hosts_path() {
    // Issue #8: validating a Linux plan from a Mac reported `brew` must not
    // run as root — for `sudo: true` apt steps, on a plan with no `brew` in
    // it. §6.5: a rule about one backend is checked where that backend is
    // named, and an unnamed one belongs to the target machine.
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    fake_manager(&bin, "brew", "exit 0\n");

    let apt_step = "- name: Install openssh-server\n  pkg:\n    name: openssh-server\n    state: present\n  sudo: true\n";
    let (code, out) = pkg_run_with_only(dir.path(), "validate", apt_step, &bin);
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains("must not run as root"), "{out}");
}

#[test]
fn a_named_manager_that_refuses_root_is_still_rejected_anywhere() {
    // The other half of the same rule: written down, it is a fact about the
    // plan, so every host agrees and `validate` says so without resolving
    // anything.
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    // No brew on PATH at all: a named manager needs no host to confirm it.
    let step = "- name: As root\n  pkg:\n    name: git\n    manager: brew\n  sudo: true\n";
    let (code, out) = pkg_run_with_only(dir.path(), "validate", step, &bin);
    assert_eq!(code, 3, "{out}");
    assert!(out.contains("`brew` must not run as root"), "{out}");
}

#[test]
fn the_host_that_resolves_brew_still_refuses_to_run_it_as_root() {
    // The rule moved, it did not go away (§6.5). On the machine whose PATH
    // actually produced brew, the step fails naming it.
    //
    // `plan`, not `apply`: the two share one `reach` body, and `apply` would
    // stop at the sudo preflight on a machine with no warm credential —
    // which is a fact about the test runner, not about this rule. Plan is
    // also the half that matters here, because a plan that promised what
    // apply refuses is the disagreement this guard exists to prevent.
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    fake_manager(&bin, "brew", "exit 0\n");

    let step = "- name: Install git\n  pkg:\n    name: git\n  sudo: true\n";
    let (code, out) = pkg_run_with_only(dir.path(), "plan", step, &bin);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("must not run as root"), "{out}");
}

#[test]
fn an_unknown_manager_lists_the_ones_that_exist() {
    let dir = tempfile::tempdir().unwrap();
    let (code, out) = pkg_plan(
        dir.path(),
        "- name: Nope\n  pkg:\n    name: git\n    manager: nix\n",
    );
    assert_eq!(code, 3, "{out}");
    assert!(out.contains("unknown package manager `nix`"), "{out}");
    for known in ["pacman", "yay", "apt", "brew", "winget"] {
        assert!(
            out.contains(known),
            "the note does not name {known}:\n{out}"
        );
    }
}

#[test]
fn cask_belongs_to_brew_and_nowhere_else() {
    let dir = tempfile::tempdir().unwrap();
    for manager in ["apt", "pacman"] {
        let (code, out) = pkg_plan(
            dir.path(),
            &format!(
                "- name: A cask\n  pkg:\n    name: git\n    manager: {manager}\n    cask: true\n"
            ),
        );
        assert_eq!(code, 3, "{out}");
        assert!(
            out.contains(&format!("`cask` does not apply to `{manager}`")),
            "{out}"
        );
        assert!(out.contains("casks are a brew concept"), "{out}");
    }
}

#[test]
fn adopt_is_a_brew_cask_install_field() {
    // Spec §6.5: `--adopt` is a brew cask install option, so it is rejected
    // where it could not be passed rather than silently doing nothing.
    let dir = tempfile::tempdir().unwrap();
    let (code, out) = pkg_plan(
        dir.path(),
        "- name: Adopt without a cask\n  pkg:\n    name: discord\n    manager: brew\n    adopt: true\n",
    );
    assert_eq!(code, 3, "{out}");
    assert!(out.contains("`adopt` requires `cask: true`"), "{out}");
    assert!(
        out.contains("--adopt is a brew cask install option"),
        "{out}"
    );

    let (code, out) = pkg_plan(
        dir.path(),
        "- name: Adopt a removal\n  pkg:\n    name: discord\n    manager: brew\n    cask: true\n    adopt: true\n    state: absent\n",
    );
    assert_eq!(code, 3, "{out}");
    assert!(
        out.contains("`adopt` does not apply to `state: absent`"),
        "{out}"
    );
}

#[test]
fn adopt_passes_adopt_to_the_brew_cask_install_call() {
    // Issue #12: a hand-installed /Applications/Discord.app makes a plain
    // cask install fail, and the workaround was a `shell` step with
    // `unless`. §6.5: `adopt: true` puts `--adopt` on the install call — and
    // on that call only, because `brew upgrade` has no such option.
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let log = dir.path().join("argv");
    let marker = dir.path().join("installed");
    fake_manager(
        &bin,
        "brew",
        &format!(
            "printf '%s\\n' \"$*\" >> {log}\n\
             case \"$*\" in\n  \
               list*) [ -f {m} ] && printf 'discord 1.0\\n'\n    exit 0 ;;\n  \
               install*) touch {m}\n    exit 0 ;;\n\
             esac\nexit 0\n",
            log = log.display(),
            m = marker.display()
        ),
    );

    let plan = "- name: A cask already on disk\n  pkg:\n    names: [discord]\n    manager: brew\n    cask: true\n    adopt: true\n";
    let (code, out) = pkg_apply_with_path(dir.path(), plan, &bin);
    assert_eq!(code, 0, "{out}");
    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(
        calls.contains("install --cask --adopt discord"),
        "the install call did not carry --adopt:\n{calls}"
    );
    // The query is `brew list --cask --versions`, and it never takes the flag.
    assert!(
        !calls
            .lines()
            .any(|l| l.starts_with("list") && l.contains("--adopt")),
        "--adopt leaked onto the query:\n{calls}"
    );

    // §6: the second run finds it installed and calls nothing.
    std::fs::write(&log, "").unwrap();
    let (code, out) = pkg_apply_with_path(dir.path(), plan, &bin);
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains("changed"), "{out}");
    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(!calls.contains("install"), "it installed again:\n{calls}");
}

#[test]
fn pkg_wants_exactly_one_of_name_and_names() {
    let dir = tempfile::tempdir().unwrap();
    let (code, out) = pkg_plan(
        dir.path(),
        "- name: Both\n  pkg:\n    name: git\n    names: [git]\n    manager: apt\n",
    );
    assert_eq!(code, 3, "{out}");
    assert!(
        out.contains("`name` and `names` are mutually exclusive"),
        "{out}"
    );

    let (code, out) = pkg_plan(dir.path(), "- name: Neither\n  pkg:\n    manager: apt\n");
    assert_eq!(code, 3, "{out}");
    assert!(out.contains("`pkg` requires `name` or `names`"), "{out}");
}

#[test]
fn an_empty_names_list_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let (code, out) = pkg_plan(
        dir.path(),
        "- name: Empty\n  pkg:\n    names: []\n    manager: apt\n",
    );
    assert_eq!(code, 3, "{out}");
    assert!(out.contains("`pkg` names no packages"), "{out}");
}

#[test]
fn a_scalar_names_field_is_rejected_not_iterated_by_character() {
    // DEFECT, reported rather than patched around: `names: git` is not a
    // list, so the render should hit the `Err(_)` arm of `try_iter` and be
    // rejected with "`names` is a list of package names". Instead minijinja's
    // string iteration (the same rule `{% for c in "abc" %}` uses) succeeds
    // silently, and `pkg` plans to install three packages named `g`, `i`,
    // `t`. `pkg::parse` never checks that the rendered value is actually a
    // sequence.
    let dir = tempfile::tempdir().unwrap();
    let (code, out) = pkg_plan(
        dir.path(),
        "- name: A scalar\n  pkg:\n    names: git\n    manager: apt\n",
    );
    assert_eq!(code, 3, "{out}");
    assert!(out.contains("`names` is a list of package names"), "{out}");
}

#[test]
fn an_unknown_state_names_the_known_ones() {
    let dir = tempfile::tempdir().unwrap();
    let (code, out) = pkg_plan(
        dir.path(),
        "- name: Bad state\n  pkg:\n    name: git\n    state: purged\n    manager: apt\n",
    );
    assert_eq!(code, 3, "{out}");
    assert!(out.contains("unknown package state `purged`"), "{out}");
    assert!(out.contains("one of: present, absent, latest"), "{out}");
}

#[test]
fn yay_is_never_chosen_as_the_default_manager() {
    // Spec §6.5: the AUR is a decision a plan has to write down. A PATH where
    // `yay` is the only manager present must still fail to resolve a default,
    // not silently pick it.
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    fake_manager(&bin, "yay", "exit 0\n");
    let path = dir.path().join("pkg.yml");
    std::fs::write(&path, "- name: No default here\n  pkg:\n    name: git\n").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_provision"))
        .args(["plan", path.to_str().expect("fixture paths are UTF-8")])
        .env("PATH", &bin)
        .env("NO_COLOR", "1")
        .output()
        .expect("provision failed to start");
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(3), "{text}");
    assert!(
        text.contains("no package manager found"),
        "yay must not be picked by default:\n{text}"
    );
}

#[test]
fn a_names_list_mixing_installed_and_missing_lists_only_the_missing_one() {
    if !apt_is_local() {
        eprintln!("skipped: needs apt");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (code, out) = pkg_plan(
        dir.path(),
        "- name: Mixed names list\n  pkg:\n    names: [coreutils, provision-no-such-package]\n    manager: apt\n",
    );
    assert_eq!(code, 2, "{out}");
    assert!(
        line_for(&out, "Mixed names list").contains("would change"),
        "{out}"
    );
    assert!(out.contains("install provision-no-such-package"), "{out}");
    assert!(
        !out.contains("install coreutils"),
        "an installed package was named too:\n{out}"
    );
}

#[test]
fn state_absent_on_an_installed_package_would_remove_it() {
    if !apt_is_local() {
        eprintln!("skipped: needs apt");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (code, out) = pkg_plan(
        dir.path(),
        "- name: Absent on an installed package\n  pkg:\n    name: coreutils\n    state: absent\n    manager: apt\n",
    );
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("remove coreutils"), "{out}");
}

#[test]
fn state_latest_on_a_missing_package_is_knowable_and_would_install() {
    // Spec §6.5: a missing package is knowable even under plan, unlike an
    // installed one, where whether a newer version exists needs the network.
    if !apt_is_local() {
        eprintln!("skipped: needs apt");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (code, out) = pkg_plan(
        dir.path(),
        "- name: Latest on a missing package\n  pkg:\n    name: provision-no-such-package\n    state: latest\n    manager: apt\n",
    );
    assert_eq!(code, 2, "{out}");
    assert_eq!(
        verdict(&out, "Latest on a missing package"),
        "would change",
        "{out}"
    );
    assert!(out.contains("install provision-no-such-package"), "{out}");
}

#[test]
fn update_cache_never_runs_a_command_under_plan() {
    // Spec §6.5: "never under plan" — proven here by making the refresh
    // command itself detectable, not by inference from the verdict.
    if !apt_is_local() {
        eprintln!("skipped: needs apt");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let marker = dir.path().join("apt-get.ran");
    fake_manager(&bin, "apt-get", &format!("touch {}\n", marker.display()));

    let path = dir.path().join("pkg.yml");
    std::fs::write(
        &path,
        "- name: Would install with update_cache\n  pkg:\n    name: provision-no-such-package\n    manager: apt\n    update_cache: true\n",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_provision"))
        .args(["plan", path.to_str().expect("fixture paths are UTF-8")])
        .env(
            "PATH",
            format!(
                "{}:{}",
                bin.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env("NO_COLOR", "1")
        .output()
        .expect("provision failed to start");
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{text}");
    assert!(text.contains("install provision-no-such-package"), "{text}");
    assert!(!marker.exists(), "apt-get ran under plan:\n{text}");
}

#[test]
fn pkg_json_status_matches_the_spec_vocabulary() {
    if !apt_is_local() {
        eprintln!("skipped: needs apt");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = fixture("pkg_plan.yml");
    let out = run_in(dir.path(), &["plan", path.to_str().unwrap(), "--json"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let steps: Vec<serde_json::Value> = stdout
        .lines()
        .map(|l| serde_json::from_str(l).expect(l))
        .filter(|r: &serde_json::Value| r["event"] == "step")
        .collect();
    assert_eq!(steps.len(), 2, "{stdout}");
    // Spec §9.3: the vocabulary is fixed, not "would change" with a space.
    assert_eq!(steps[0]["status"], "would_change", "{stdout}");
    assert_eq!(steps[1]["status"], "unknown", "{stdout}");
}

// ── pkg parsers, from captured manager output ──────────────────────────────
//
// Every row's query parser is exercised by putting a stand-in for the
// manager's binary on PATH ahead of anything real, so it can answer with
// output captured from a real run without a second machine to run it on.
// Only the query is stubbed — plan never runs the manager for anything
// else — and what is under test either way is the real parser in
// src/actions/pkg.rs, reached through the real `pkg` action.

fn fake_manager(bin_dir: &Path, name: &str, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    let path = bin_dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n"))
        .expect("writing a stub into the test's own tempdir");
    let mut perm = std::fs::metadata(&path)
        .expect("just written")
        .permissions();
    perm.set_mode(0o755);
    std::fs::set_permissions(&path, perm).expect("just written");
}

/// Cats a captured-output fixture back out, whatever the manager was called
/// with — plan only ever asks it to query.
fn cat_fixture(name: &str) -> String {
    let text =
        std::fs::read_to_string(fixture(&format!("pkg/{name}"))).expect("a checked-in fixture");
    format!("cat <<'PKGFIXTURE'\n{text}PKGFIXTURE\n")
}

fn pkg_plan_with_path(dir: &Path, body: &str, bin_dir: &Path) -> (i32, String) {
    let path = dir.join("pkg.yml");
    std::fs::write(&path, body).expect("writing a plan into the test's own tempdir");
    let out = Command::new(env!("CARGO_BIN_EXE_provision"))
        .args(["plan", path.to_str().expect("fixture paths are UTF-8")])
        .env(
            "PATH",
            format!(
                "{}:{}",
                bin_dir.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env("NO_COLOR", "1")
        .output()
        .expect("provision failed to start");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
    )
}

#[test]
fn parse_dpkg_keeps_only_the_install_ok_installed_stanza() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    fake_manager(&bin, "dpkg-query", &cat_fixture("dpkg_query.txt"));
    // The apt manager checks for `apt-get` before it queries `dpkg-query`,
    // and `pkg_plan_with_path` prepends this directory to the real PATH
    // rather than replacing it — so without a stub here the test read the
    // host's apt, passed on Debian, and failed everywhere else. Nothing runs
    // it: `plan` only ever queries.
    fake_manager(&bin, "apt-get", "exit 0");

    let (code, out) = pkg_plan_with_path(
        dir.path(),
        "- name: Mixed dpkg stanzas\n  pkg:\n    names: [sl, nano, ghost]\n    manager: apt\n",
        &bin,
    );
    assert_eq!(code, 2, "{out}");
    // `nano` is config-files-only and `ghost` is a status this table never
    // emits for real; neither counts as installed, only `sl` does.
    assert!(out.contains("install nano ghost"), "{out}");
    assert!(
        !out.contains("install sl"),
        "the installed stanza was misread:\n{out}"
    );
}

#[test]
fn parse_space_pairs_reads_a_pacman_query() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    fake_manager(&bin, "pacman", &cat_fixture("pacman_q.txt"));

    let (code, out) = pkg_plan_with_path(
        dir.path(),
        "- name: A pacman query\n  pkg:\n    names: [bash, coreutils, tree]\n    manager: pacman\n",
        &bin,
    );
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("install tree"), "{out}");
    assert!(
        !out.contains("install bash") && !out.contains("install coreutils"),
        "{out}"
    );
}

/// Apply with a stubbed manager on PATH. Unlike `pkg_plan_with_path`, this
/// one lets the stub mutate: apply is the only mode that calls install.
fn pkg_apply_with_path(dir: &Path, body: &str, bin_dir: &Path) -> (i32, String) {
    let path = dir.join("pkg.yml");
    std::fs::write(&path, body).expect("writing a plan into the test's own tempdir");
    let out = Command::new(env!("CARGO_BIN_EXE_provision"))
        .args(["apply", path.to_str().expect("fixture paths are UTF-8")])
        .env(
            "PATH",
            format!(
                "{}:{}",
                bin_dir.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env("NO_COLOR", "1")
        .output()
        .expect("provision failed to start");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
    )
}

// Spec §6.5: `changed` means missing before *and present after*. The live
// case that found this: `apt-get install -y yarn` exits 0 on Debian and
// installs nothing, because `yarn` is a virtual package `cmdtest` provides,
// and `dpkg-query` still reports the name absent. Reported as `changed` the
// step never converges — every apply on main_pc claimed the same install.
#[test]
fn an_install_that_did_nothing_fails_instead_of_claiming_changed() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    // The virtual package: the query never lists it, whatever apt-get says.
    fake_manager(
        &bin,
        "dpkg-query",
        "printf 'cmdtest\t0.32\tinstall ok installed\n'
",
    );
    fake_manager(
        &bin, "apt-get", "exit 0
",
    );

    let (code, out) = pkg_apply_with_path(
        dir.path(),
        "- name: A virtual package\n  pkg:\n    names: [yarn]\n    manager: apt\n",
        &bin,
    );
    assert_eq!(code, 1, "a no-op install was not a failure:\n{out}");
    assert!(
        out.contains("yarn still not installed after `apt-get install`"),
        "{out}"
    );
    assert!(
        !out.contains("install yarn"),
        "it still claimed the install:\n{out}"
    );
}

// The same rule under `state: latest`, which installs what is missing before
// upgrading the rest. That path compared versions before and after and never
// asked whether the install had installed anything: yarn missing before and
// missing after is "no version moved", so the step read `ok` — every apply,
// with plan saying `install yarn` each time.
#[test]
fn latest_fails_an_install_that_did_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    fake_manager(
        &bin,
        "dpkg-query",
        "printf 'cmdtest\t0.32\tinstall ok installed\n'
",
    );
    fake_manager(
        &bin, "apt-get", "exit 0
",
    );

    let (code, out) = pkg_apply_with_path(
        dir.path(),
        "- name: A virtual package, latest\n  pkg:\n    names: [yarn]\n    manager: apt\n    state: latest\n",
        &bin,
    );
    assert_eq!(
        code, 1,
        "a no-op install under latest was not a failure:\n{out}"
    );
    assert!(
        out.contains("yarn still not installed after `apt-get install`"),
        "{out}"
    );
}

// Issue #71: `cask` is checked at validate only where the manager is named.
// An unnamed one is met on the host, and `reach` is where that is said —
// otherwise the flag is silently dropped and the plain package installs.
#[test]
fn cask_on_an_unnamed_manager_that_has_none_fails_on_the_host() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    fake_manager(&bin, "apt-get", "exit 0");
    fake_manager(&bin, "dpkg-query", "exit 0");

    let step = "- name: A cask\n  pkg:\n    name: git\n    cask: true\n";
    let (code, out) = pkg_run_with_only(dir.path(), "validate", step, &bin);
    assert_eq!(code, 0, "{out}");
    for verb in ["plan", "apply"] {
        let (code, out) = pkg_run_with_only(dir.path(), verb, step, &bin);
        assert_eq!(code, 1, "{verb}:\n{out}");
        assert!(
            out.contains("`cask` does not apply to `apt`"),
            "{verb}:\n{out}"
        );
    }
}

// Issue #72: spec §6.5 scopes `update_cache` to the install call, and only
// when there is something to install. A removal does not need fresh lists.
#[test]
fn update_cache_does_not_refresh_before_a_removal() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let log = dir.path().join("apt-get.log");
    fake_manager(
        &bin,
        "dpkg-query",
        "printf 'held\\t1.0\\tinstall ok installed\\n'",
    );
    fake_manager(
        &bin,
        "apt-get",
        &format!("echo \"$@\" >> {}", log.display()),
    );

    let (_, out) = pkg_apply_with_path(
        dir.path(),
        "- name: Remove\n  pkg:\n    name: held\n    state: absent\n    manager: apt\n    update_cache: true\n",
        &bin,
    );
    let calls = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(calls.contains("remove"), "no removal ran:\n{calls}\n{out}");
    assert!(
        !calls.contains("update"),
        "refreshed for a removal:\n{calls}"
    );
}

// `latest` with one missing and one held name makes two calls; the refresh
// belongs to the step, not to each call.
#[test]
fn update_cache_refreshes_once_per_step() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let log = dir.path().join("apt-get.log");
    let installed = dir.path().join("fresh.installed");
    fake_manager(
        &bin,
        "dpkg-query",
        &format!(
            "printf 'held\\t1.0\\tinstall ok installed\\n'\n[ -e {} ] && printf 'fresh\\t1.0\\tinstall ok installed\\n'\nexit 0",
            installed.display()
        ),
    );
    fake_manager(
        &bin,
        "apt-get",
        &format!(
            "echo \"$@\" >> {}\ncase \"$*\" in *install*fresh*) : > {};; esac",
            log.display(),
            installed.display()
        ),
    );

    let (code, out) = pkg_apply_with_path(
        dir.path(),
        "- name: Latest\n  pkg:\n    names: [held, fresh]\n    state: latest\n    manager: apt\n    update_cache: true\n",
        &bin,
    );
    let calls = std::fs::read_to_string(&log).unwrap_or_default();
    assert_eq!(code, 0, "{out}\n{calls}");
    assert!(calls.contains("--only-upgrade"), "no upgrade ran:\n{calls}");
    assert_eq!(
        calls.lines().filter(|l| l.contains("update")).count(),
        1,
        "{calls}"
    );
}

// Spec §6.5: the query is where every verdict comes from, and a query that
// failed gave no answer. Its exit code was never read, so a `dpkg-query` that
// errored with empty output read as "nothing installed": `state: absent`
// reported `ok` without removing anything, and `present` installed and then
// blamed the install for what the query got wrong.
#[test]
fn a_failed_query_fails_the_step() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    fake_manager(
        &bin,
        "dpkg-query",
        "echo 'dpkg-query: the database is locked' >&2
exit 2
",
    );
    fake_manager(
        &bin, "apt-get", "exit 0
",
    );

    let (code, out) = pkg_apply_with_path(
        dir.path(),
        "- name: Removes git\n  pkg:\n    names: [git]\n    manager: apt\n    state: absent\n",
        &bin,
    );
    assert_eq!(code, 1, "a failed query read as nothing installed:\n{out}");
    assert!(out.contains("the database is locked"), "{out}");
}

// The same rule one command further in. A typed action runs several — a
// `pkg` step queries, then installs — and the budget used to be taken once,
// before the first of them, then handed to each in turn. So the step could
// spend the whole remaining deadline on the query and the whole of it again
// on the install. Here the query answers slowly but successfully, so the
// install is reached with the clock nearly out and must not get a fresh
// second of it.
//
// The deadline is 10s against a 2s query, not the 3s this was written with.
// Three left the query about a second of room, and a host slow enough to eat
// it spends the whole deadline before the install is ever reached — at which
// point there is no second command to measure and the test is green about
// nothing (#64). Ten leaves eight seconds of spawn slack. The gap being read
// is the query's two seconds either way.
#[test]
fn a_typed_actions_second_command_gets_what_is_left_after_its_first() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    // Answers "nothing installed" after two seconds, so the install is next
    // with most of the budget gone. The gap the assertion reads is two seconds
    // wide by construction: reclamped, the install is killed with the ~8s that
    // is left and the step ends near 10s; given the budget again, it is killed
    // after another 10s and the step ends near 12s.
    fake_manager(
        &bin,
        "dpkg-query",
        "sleep 2
exit 0
",
    );
    // And the install hangs. With the budget reclamped it is killed with
    // what little is left; without, it was given the whole of it again.
    fake_manager(
        &bin,
        "apt-get",
        "sleep 20
",
    );
    let path = dir.path().join("pkg.yml");
    std::fs::write(
        &path,
        "- name: An install behind a slow query
  pkg:
    names: [yarn]
    manager: apt
",
    )
    .unwrap();

    let started = std::time::Instant::now();
    let out = Command::new(env!("CARGO_BIN_EXE_provision"))
        .args([
            "apply",
            path.to_str().expect("temp paths are UTF-8"),
            "--deadline",
            "10s",
            "--json",
        ])
        .env(
            "PATH",
            format!(
                "{}:{}",
                bin.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env("NO_COLOR", "1")
        .output()
        .expect("provision failed to start");
    let elapsed = started.elapsed();
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);

    assert_eq!(out.status.code(), Some(124), "{text}");
    // The step's own `duration_ms`, not wall clock. The gap is two seconds
    // wide by construction -- reclamped the step ends near 10s, given the
    // budget twice it ends near 12s -- but a wall-clock reading of it also
    // carries process startup and this binary's own build-and-launch, which
    // on a loaded machine ate most of the margin and failed the test for
    // reasons that had nothing to do with the clamp. provision measures the
    // step itself, so the noise is not in the number.
    let step_ms = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|r| r["event"] == "step")
        .and_then(|r| r["duration_ms"].as_u64())
        .expect("the step reported a duration");
    assert!(
        step_ms < 11000,
        "the second command was given the budget the first had already spent: {step_ms}ms\n{text}"
    );
    // The outer bound, loose on purpose: it catches a run that hung without
    // competing with the assertion above for the same margin.
    assert!(
        elapsed < std::time::Duration::from_secs(30),
        "the run did not end: {elapsed:?}\n{text}"
    );
}

// Spec §8, line "`--deadline` runs out during a step": a kill the run's clock
// caused is worded `deadline exceeded`, never as a step timeout the file does
// not contain. The flag deciding that wording was read once, before the
// action's first command, while the duration was reclamped per command — so a
// step with its own `timeout:` entered unbound by the deadline, was clamped by
// it on a later command, and reported a step timeout wording after a number
// neither the file's `timeout:` nor the run's deadline holds.
//
// The sibling above cannot catch it — it declares no step `timeout:`, so the
// deadline is the smaller from the start and the flag is right by accident.
//
// The three numbers are a squeeze and they were set too tight (#64). What
// this needs is for the install to start with time left on the run's clock
// but less of it than its own ceiling: 0 < remaining < `timeout:`. The upper
// half is free — load only ever makes `remaining` smaller — so the whole
// margin is the lower half, and that is `deadline` minus the query. At 6s
// against a 3s query it was three seconds, and a host that lost three
// seconds to spawn let the deadline fire before the install was reached,
// changing the wording the test reads. At 12s against a 2s query it is ten.
// `timeout:` is 11s, one second above the ~10s that will be left, so it
// stays the losing clock whichever way the host drifts.
#[test]
fn a_deadline_kill_is_not_worded_as_the_steps_own_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    // Answers "nothing installed", slowly but inside its own budget: the
    // query is bound by the step's `timeout:`, not by the run's clock.
    fake_manager(
        &bin,
        "dpkg-query",
        "sleep 2
exit 0
",
    );
    // The install is reached with ~10s left against an 11s ceiling, so it is
    // the deadline that binds it and the deadline that kills it.
    fake_manager(
        &bin,
        "apt-get",
        "sleep 20
",
    );
    let path = dir.path().join("pkg.yml");
    std::fs::write(
        &path,
        "- name: An install behind a slow query, under its own timeout
  timeout: 11s
  pkg:
    names: [yarn]
    manager: apt
",
    )
    .unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_provision"))
        .args([
            "apply",
            path.to_str().expect("temp paths are UTF-8"),
            "--deadline",
            "12s",
        ])
        .env(
            "PATH",
            format!(
                "{}:{}",
                bin.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env("NO_COLOR", "1")
        .output()
        .expect("provision failed to start");
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);

    assert_eq!(out.status.code(), Some(124), "{text}");
    assert!(
        text.contains("deadline exceeded after"),
        "the run's clock killed the step and the message named the step's own timeout:\n{text}"
    );
    assert!(
        !text.contains("timed out after"),
        "a deadline kill was worded as a step timeout:\n{text}"
    );
}

// Spec §8: each step's effective timeout is the lesser of its own and the
// time left, so no step runs past the deadline it was admitted under. A typed
// action took the timeout computed before its `unless` ran — so the gate's
// two seconds were spent twice, and the hung query below ran to ~8s against a
// 6s deadline and said `timed out` about a step timeout the file never wrote.
//
// The deadline is 6s against a 2s gate, not the 3s this was written with.
// Three left the gate about a second of room: a slow enough host spent the
// whole deadline inside the gate, the query never ran at all, and the step
// failed as `unless` deadline exceeded — a green test's premise quietly gone
// (#64). Six leaves four seconds of spawn slack and does not change what is
// being measured, because the gap under test is the gate's two seconds
// wherever the deadline is set.
#[test]
fn a_typed_action_after_its_gate_gets_only_the_time_left() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    // The query is the first command a `pkg` step runs, and this one hangs.
    // `apt-get` only has to be on PATH: the step checks for it first.
    // Each stub logs when it was called and when it stopped, so a failure
    // here says *which* command spent the time rather than only that
    // something did. Without this the overrun is a number with no owner, and
    // the number is the whole finding.
    let log = dir.path().join("calls.log");
    let stamp = |what: &str| {
        format!(
            "start=$(date +%s.%N); echo \"{what} start $start\" >> {0}",
            log.display()
        )
    };
    fake_manager(
        &bin,
        "dpkg-query",
        &format!("{}\nsleep 20\n", stamp("dpkg-query")),
    );
    fake_manager(&bin, "apt-get", &format!("{}\nexit 0\n", stamp("apt-get")));
    let path = dir.path().join("pkg.yml");
    std::fs::write(
        &path,
        format!(
            "- name: A query that hangs behind a slow gate\n  pkg:\n    names: [yarn]\n    manager: apt\n  unless: \"echo gate $(date +%s.%N) >> '{0}'; sleep 2; exit 1\"\n",
            log.display()
        ),
    )
    .unwrap();

    // Wall clock as well as the monotonic one: the log below is stamped in
    // epoch seconds, and the question a failure has to answer is *where* the
    // time went — before the gate, between the gate and the query, or after
    // the kill. Only an absolute start makes those three distinguishable.
    // Diagnostic only: nothing is asserted against it.
    let wall = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or_default();
    let started = std::time::Instant::now();
    let out = Command::new(env!("CARGO_BIN_EXE_provision"))
        .args([
            "apply",
            path.to_str().expect("temp paths are UTF-8"),
            "--deadline",
            "6s",
            "--json",
        ])
        .env(
            "PATH",
            format!(
                "{}:{}",
                bin.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env("NO_COLOR", "1")
        .output()
        .expect("provision failed to start");
    let elapsed = started.elapsed();
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);

    assert_eq!(out.status.code(), Some(124), "{text}");
    let calls = std::fs::read_to_string(&log).unwrap_or_default();
    // The step's own `duration_ms`, not wall clock — the same reading
    // `a_typed_actions_second_command_gets_what_is_left_after_its_first`
    // takes, and for the same reason. The gap is the gate's two seconds by
    // construction: reclamped the step ends at the 6s deadline, given the
    // budget a second time it ends near 8s. A wall-clock reading of that gap
    // also carries process spawn and this binary's launch, which on a loaded
    // host ate the margin and failed the test for reasons that had nothing to
    // do with the clamp (#64). provision measures the step itself, so the
    // noise is not in the number.
    let step_ms = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|r| r["event"] == "step")
        .and_then(|r| r["duration_ms"].as_u64())
        .expect("the step reported a duration");
    assert!(
        step_ms < 7000,
        "the query was given the whole deadline instead of what the gate left: {step_ms}ms\nstarted at {wall:.3}\ncommands the step ran:\n{calls}\n{text}"
    );
    assert!(
        text.contains("deadline exceeded after"),
        "named a step timeout the file does not contain:\n{text}"
    );
    // Loose on purpose: this only catches a run that never ended, and must
    // not compete with the assertion above for the same margin.
    assert!(
        elapsed < std::time::Duration::from_secs(30),
        "the run did not end: {elapsed:?}\n{text}"
    );
}

// The other half: an install that really installs is still `changed`. Without
// this the fix above could pass by failing every install there is.
#[test]
fn an_install_that_worked_is_still_changed() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let marker = dir.path().join("installed");
    // The query answers from the marker the install call leaves behind, so
    // the second query in the same step sees what the first one did not.
    fake_manager(
        &bin,
        "dpkg-query",
        &format!(
            "[ -f {m} ] && printf 'tree\t1.8\tinstall ok installed\n'\nexit 0\n",
            m = marker.display()
        ),
    );
    fake_manager(&bin, "apt-get", &format!("touch {}\n", marker.display()));

    let (code, out) = pkg_apply_with_path(
        dir.path(),
        "- name: A real package\n  pkg:\n    names: [tree]\n    manager: apt\n",
        &bin,
    );
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("install tree"), "{out}");
}

fn fake_brew(bin_dir: &Path) {
    let body = format!(
        "case \"$*\" in\n  *--cask*)\n{}    ;;\n  *)\n{}    ;;\nesac\n",
        cat_fixture("brew_cask_versions.txt"),
        cat_fixture("brew_formula_versions.txt"),
    );
    fake_manager(bin_dir, "brew", &body);
}

#[test]
fn parse_space_pairs_takes_the_first_version_from_a_brew_query() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    fake_brew(&bin);

    let (code, out) = pkg_plan_with_path(
        dir.path(),
        "- name: A brew formula query\n  pkg:\n    names: [jq, wget, curl]\n    manager: brew\n",
        &bin,
    );
    assert_eq!(code, 2, "{out}");
    // `wget` lists two versions on one line; the first is enough to read it
    // as installed, and only `curl` is genuinely missing.
    assert!(out.contains("install curl"), "{out}");
    assert!(
        !out.contains("install jq") && !out.contains("install wget"),
        "{out}"
    );
}

#[test]
fn brew_reads_the_cask_list_when_cask_is_true() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    fake_brew(&bin);

    let (code, out) = pkg_plan_with_path(
        dir.path(),
        "- name: A brew cask query\n  pkg:\n    names: [docker, firefox]\n    manager: brew\n    cask: true\n",
        &bin,
    );
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("install firefox"), "{out}");
    assert!(
        !out.contains("install docker"),
        "the cask list was not consulted:\n{out}"
    );
}

#[test]
fn brew_matches_a_tap_qualified_name_by_its_last_segment() {
    // 7151a8a: `hashicorp/tap/terraform` never converged, because `brew list
    // --versions` reports it as plain `terraform`.
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    fake_brew(&bin);

    let (code, out) = pkg_plan_with_path(
        dir.path(),
        "- name: Tap-qualified names\n  pkg:\n    names: [hashicorp/tap/terraform, hashicorp/tap/packer, hashicorp/tap/vault]\n    manager: brew\n",
        &bin,
    );
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("install hashicorp/tap/vault"), "{out}");
    assert!(
        !out.contains("terraform"),
        "a tap-qualified name that is installed was re-offered:\n{out}"
    );
    assert!(!out.contains("hashicorp/tap/packer"), "{out}");
}

#[test]
fn parse_winget_cuts_columns_by_the_headers_offsets() {
    // The fixture's table has a two-word name, a row with no Version, and a
    // non-ASCII name — the case that would panic or misalign under a
    // byte-offset cut instead of the char-based one the row actually uses.
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    fake_manager(&bin, "winget", &cat_fixture("winget_list.txt"));

    let (code, out) = pkg_plan_with_path(
        dir.path(),
        "- name: A winget table\n  pkg:\n    names: [Google.Chrome, Microsoft.VisualStudioCode, 7zip.7zip, Cafe.MusicPlayer, Nonexistent.App]\n    manager: winget\n",
        &bin,
    );
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("install Nonexistent.App"), "{out}");
    for id in [
        "Google.Chrome",
        "Microsoft.VisualStudioCode",
        "7zip.7zip",
        "Cafe.MusicPlayer",
    ] {
        assert!(
            !out.contains(&format!("install {id}")),
            "{id} misread as missing:\n{out}"
        );
    }
}

// The apt status filter — `dpkg-query -W` alone also lists removed-but-config
// packages, which would read as present — has no host-side test: every one of
// the 1660 entries in this machine's database is `install ok installed`, so
// there is nothing here to catch the missing filter. The assertion lives in
// `the_apt_query_ignores_a_package_that_is_only_config_files`, which makes
// such a package inside a container. plan.md records that.

#[test]
fn a_typed_actions_command_is_bound_by_the_steps_timeout() {
    // 837a07b. Before it a command a typed action spawned carried no
    // deadline, so a manager blocked on a lock hung the run for good.
    //
    // What hangs here is `dpkg-query`: the real query of the real `pkg`
    // action, reached through the real `Ctx::exec`, supplied by a stub
    // earlier on PATH. Only the binary is a stand-in — a manager that blocks
    // on its lock cannot be arranged on this host without touching its
    // packages. Nothing about the verdict is simulated.
    if !apt_is_local() {
        eprintln!("skipped: needs apt");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let pidfile = dir.path().join("query.pid");
    let stub = bin.join("dpkg-query");
    std::fs::write(
        &stub,
        format!(
            "#!/bin/sh\nsleep 300 &\necho $! > {}\nsleep 300\n",
            pidfile.display()
        ),
    )
    .unwrap();
    set_mode(&stub, 0o755);

    let plan = dir.path().join("hang.yml");
    std::fs::write(
        &plan,
        "- name: A query that never answers\n  pkg:\n    name: coreutils\n    manager: apt\n  timeout: 2s\n",
    )
    .unwrap();

    let began = std::time::Instant::now();
    let out = Command::new(env!("CARGO_BIN_EXE_provision"))
        .args(["apply", plan.to_str().unwrap()])
        .env(
            "PATH",
            format!(
                "{}:{}",
                bin.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env("NO_COLOR", "1")
        .output()
        .expect("provision failed to start");
    let elapsed = began.elapsed();
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);

    assert_eq!(out.status.code(), Some(1), "{text}");
    // The step's own 2s, not the ten-minute default of spec §4, and worded
    // the way the shell path words it.
    assert!(text.contains("timed out after 2"), "{text}");
    assert!(
        elapsed < std::time::Duration::from_mins(1),
        "the step was never killed: {elapsed:?}"
    );

    // And the kill reached past the command into what the command started.
    let pid = std::fs::read_to_string(&pidfile)
        .expect("the stub query never ran")
        .trim()
        .to_string();
    let alive = Command::new("kill")
        .args(["-0", &pid])
        .output()
        .is_ok_and(|o| o.status.success());
    assert!(
        !alive,
        "pid {pid} outlived the process group it was killed with"
    );
}

// ── pkg, in a container ───────────────────────────────────────────────────

/// Installing and removing packages is the one thing these tests may not do
/// to the machine they run on, so it happens in a throwaway container or not
/// at all. Off by default: a plain `cargo test` must not need Docker.
fn container_tests_enabled() -> bool {
    if std::env::var("PROVISION_CONTAINER_TESTS").is_err() {
        eprintln!("skipped: set PROVISION_CONTAINER_TESTS=1");
        return false;
    }
    // `in_container` mounts *this host's* test binary into the image, so the
    // host has to be the platform the image runs. From a macOS host every one
    // of these fails as `sh: 1: provision: Exec format error`, which says
    // nothing about what is wrong or that nothing is wrong with provision.
    // The fleet's only Linux machines are containers and the CI runner, so
    // this is the common case rather than the odd one.
    if !cfg!(target_os = "linux") {
        eprintln!(
            "skipped: the container mounts this host's test binary, so these run \
             from a Linux host only"
        );
        return false;
    }
    true
}

/// One shell line in a throwaway container, with the binary under test and
/// the fixture directory mounted in. `--rm`, so nothing survives the run.
fn in_container(image: &str, script: &str) -> (i32, String) {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/apply");
    let args: Vec<String> = vec![
        "run".into(),
        "--rm".into(),
        "-v".into(),
        format!(
            "{}:/usr/local/bin/provision:ro",
            env!("CARGO_BIN_EXE_provision")
        ),
        "-v".into(),
        format!("{}:/plan:ro", fixtures.display()),
        "-e".into(),
        "NO_COLOR=1".into(),
        "-e".into(),
        "DEBIAN_FRONTEND=noninteractive".into(),
        image.into(),
        "sh".into(),
        "-c".into(),
        script.into(),
    ];
    let out = Command::new("docker")
        .args(&args)
        .output()
        .expect("docker failed to start");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
    )
}

/// The two applies of the idempotency gate, in one container so the first
/// one's state is what the second one reads.
fn twice(image: &str, plan: &str) -> (i32, String) {
    in_container(
        image,
        &format!(
            "provision apply /plan/{plan} && echo ---SECOND--- && provision apply /plan/{plan}"
        ),
    )
}

#[test]
#[ignore = "needs docker; set PROVISION_CONTAINER_TESTS=1"]
fn apt_installs_a_package_twice_changed_then_ok() {
    if !container_tests_enabled() {
        return;
    }
    let (code, out) = twice("ubuntu:24.04", "pkg_container_apt.yml");
    assert_eq!(code, 0, "{out}");
    let (first, second) = out.split_once("---SECOND---").expect(&out);
    assert!(
        line_for(first, "A tiny package").contains("changed"),
        "{out}"
    );
    assert!(first.contains("install sl"), "{out}");
    assert_eq!(verdict(second, "A tiny package"), "ok", "{out}");
}

#[test]
#[ignore = "needs docker; set PROVISION_CONTAINER_TESTS=1"]
fn pacman_installs_a_package_twice_changed_then_ok() {
    if !container_tests_enabled() {
        return;
    }
    let (code, out) = twice("archlinux:latest", "pkg_container_pacman.yml");
    assert_eq!(code, 0, "{out}");
    let (first, second) = out.split_once("---SECOND---").expect(&out);
    assert!(
        line_for(first, "A tiny package").contains("changed"),
        "{out}"
    );
    assert!(first.contains("install tree"), "{out}");
    assert_eq!(verdict(second, "A tiny package"), "ok", "{out}");
}

#[test]
#[ignore = "needs docker; set PROVISION_CONTAINER_TESTS=1"]
fn the_apt_query_ignores_a_package_that_is_only_config_files() {
    // Spec §6.5: plain `dpkg-query -W` lists a removed-but-config package,
    // which would read as present and make `present` a silent no-op on a
    // package that is not installed. `nano` leaves /etc/nanorc behind, so
    // install-then-remove is enough to build the case. No host has one to
    // borrow, so it is built here.
    if !container_tests_enabled() {
        return;
    }
    let (code, out) = in_container(
        "ubuntu:24.04",
        "apt-get update -qq >/dev/null && apt-get install -y -qq nano >/dev/null \
         && apt-get remove -y -qq nano >/dev/null \
         && dpkg-query -W -f '${Package}\\t${Status}\\n' nano \
         && provision plan /plan/pkg_container_config_files.yml",
    );
    assert!(
        out.contains("deinstall ok config-files"),
        "the case was not built:\n{out}"
    );
    assert_eq!(code, 2, "{out}");
    assert!(
        line_for(&out, "only config files").contains("would change"),
        "{out}"
    );
    assert!(out.contains("install nano"), "{out}");
}

#[test]
#[ignore = "needs docker; set PROVISION_CONTAINER_TESTS=1"]
fn a_pacman_names_list_installs_only_the_missing_one() {
    if !container_tests_enabled() {
        return;
    }
    let (code, out) = in_container(
        "archlinux:latest",
        "provision apply /plan/pkg_container_pacman_mixed.yml",
    );
    assert_eq!(code, 0, "{out}");
    assert!(line_for(&out, "already there").contains("changed"), "{out}");
    assert!(out.contains("install tree"), "{out}");
    assert!(
        !out.contains("install pacman"),
        "the preinstalled package was named too:\n{out}"
    );
}

#[test]
#[ignore = "needs docker; set PROVISION_CONTAINER_TESTS=1"]
fn state_latest_installs_a_package_that_was_never_there() {
    // 7151a8a. Proven inside the one container the apply ran in, since a
    // fresh `--rm` container remembers nothing between two `in_container`
    // calls.
    if !container_tests_enabled() {
        return;
    }
    let (code, out) = in_container(
        "ubuntu:24.04",
        "provision apply /plan/pkg_container_latest_missing.yml \
         && dpkg -s sl >/dev/null 2>&1 && echo REALLY-INSTALLED",
    );
    assert_eq!(code, 0, "{out}");
    assert!(
        line_for(&out, "not yet installed").contains("changed"),
        "{out}"
    );
    assert!(
        out.contains("REALLY-INSTALLED"),
        "sl was reported changed but is not there:\n{out}"
    );
}

#[test]
#[ignore = "needs docker; set PROVISION_CONTAINER_TESTS=1"]
fn update_cache_refreshes_only_when_there_is_something_to_install() {
    // A typed action never surfaces the manager's own chatter (only `shell`
    // and `cmd` forward raw child output — spec §9), so `apt-get update`
    // running is not something provision's own text will ever show. The
    // ubuntu:24.04 image ships with `/var/lib/apt/lists` stripped empty to
    // save space, so whether it gained real list files is the manager's own
    // side effect, read directly.
    if !container_tests_enabled() {
        return;
    }
    let lists = "ls /var/lib/apt/lists | grep -v -E '^(lock|partial|auxfiles)$' | wc -l";
    let (code, noop) = in_container(
        "ubuntu:24.04",
        &format!("provision apply /plan/pkg_container_update_cache_noop.yml && {lists}"),
    );
    assert_eq!(code, 0, "{noop}");
    assert_eq!(
        noop.trim_end().lines().last(),
        Some("0"),
        "refreshed with nothing to do:\n{noop}"
    );

    let (code, installs) = in_container(
        "ubuntu:24.04",
        &format!("provision apply /plan/pkg_container_apt.yml && {lists}"),
    );
    assert_eq!(code, 0, "{installs}");
    assert_ne!(
        installs.trim_end().lines().last(),
        Some("0"),
        "did not refresh before installing:\n{installs}"
    );
}

// A container test holding a real dpkg lock with the `flock` utility and
// expecting a real `apt-get install` to block on it was tried and dropped:
// GNU `flock` takes a BSD `flock(2)` lock, dpkg/apt take a POSIX `fcntl(2)`
// lock on the same file, and the two are independent locking systems in
// Linux — one never contends with the other. `apt-get install` ran straight
// through every time, and the resulting test was pure timing noise (it
// passed or failed by how long the container took to start, not by whether
// anything actually blocked). `a_typed_actions_command_is_bound_by_the_steps_timeout`
// already proves the kill-on-timeout mechanism deterministically, through
// the real `pkg` action and the real `Ctx::exec`; only the query's binary is
// a stand-in there, and it is the mechanism under test either way.

// ── `git` (spec §6.8) ─────────────────────────────────────────────────────

/// A bare repository with the three ref shapes §6.8 distinguishes: a
/// lightweight tag, an annotated tag, and a branch that moves.
///
/// The annotated tag is not one case among three. Its ref resolves to the tag
/// object rather than to the commit, so a comparison without `^{commit}`
/// reports `changed` on every run forever — which reads exactly like working.
struct Remote {
    dir: tempfile::TempDir,
}

impl Remote {
    fn new() -> Remote {
        let dir = tempfile::tempdir().expect("a temp dir for the remote");
        let r = Remote { dir };
        git_at(
            r.root(),
            &["init", "--quiet", "--bare", "-b", "main", &r.url()],
        );
        // A working clone to build the history in, thrown away after.
        let work = r.root().join("work");
        r.clone_into(&work);
        commit(&work, "one");
        git_at(&work, &["tag", "light"]);
        commit(&work, "two");
        git_at(&work, &["tag", "-a", "annotated", "-m", "an annotated tag"]);
        commit(&work, "three");
        git_at(&work, &["push", "--quiet", "--tags", "origin", "main"]);
        discard(&work);
        r
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn path(&self) -> PathBuf {
        self.root().join("remote.git")
    }

    fn url(&self) -> String {
        self.path().display().to_string()
    }

    /// One more commit on `main`, pushed. This is how the branch moves under
    /// a checkout that already exists.
    fn advance(&self) {
        let work = self.root().join("advance");
        self.clone_into(&work);
        commit(&work, "four");
        git_at(&work, &["push", "--quiet", "origin", "main"]);
        discard(&work);
    }

    /// A tag created after a checkout was made, so the checkout has never
    /// heard of it. It points at `at` rather than at the tip, so converging
    /// on it has to move HEAD — a tag on the commit the checkout already sits
    /// on would read `ok` and prove nothing about the fetch.
    fn tag_later(&self, name: &str, at: &str) {
        let work = self.root().join("tagging");
        self.clone_into(&work);
        git_at(&work, &["tag", "-a", name, "-m", "later", at]);
        git_at(&work, &["push", "--quiet", "--tags", "origin"]);
        discard(&work);
    }

    fn clone_into(&self, work: &Path) {
        let into = work.display().to_string();
        git_at(self.root(), &["clone", "--quiet", &self.url(), &into]);
    }
}

/// One commit adding or rewriting the single file the history carries.
fn commit(work: &Path, text: &str) {
    std::fs::write(work.join("file.txt"), format!("{text}\n"))
        .expect("write the file the commits carry");
    git_at(work, &["add", "file.txt"]);
    git_at(work, &["commit", "--quiet", "-m", text]);
}

/// `git` in a directory, with an author and committer the suite supplies: a
/// machine running the tests may have no identity configured, and `commit`
/// would fail on it for a reason that has nothing to do with the test.
fn git_at(cwd: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "provision tests")
        .env("GIT_AUTHOR_EMAIL", "tests@example.invalid")
        .env("GIT_COMMITTER_NAME", "provision tests")
        .env("GIT_COMMITTER_EMAIL", "tests@example.invalid")
        .output()
        .expect("git failed to start");
    assert!(
        out.status.success(),
        "git {args:?} failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn discard(work: &Path) {
    std::fs::remove_dir_all(work).expect("remove the working clone");
}

/// `git rev-parse` in a checkout the test made, for asserting where HEAD went.
fn head_of(dir: &Path) -> String {
    rev_of(dir, "HEAD")
}

/// Any revision, so a test can pin something that is not the tip. Empty when
/// git could not answer, which every caller asserts against.
fn rev_of(dir: &Path, rev: &str) -> String {
    let at = dir.display().to_string();
    let out = Command::new("git")
        .args(["-C", &at, "rev-parse", rev])
        .output()
        .expect("git failed to start");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Is this checkout shallow, and how much history did it take?
fn shallow_depth(dir: &Path) -> (bool, usize) {
    let shallow = rev_of(dir, "--is-shallow-repository") == "true";
    let at = dir.display().to_string();
    let out = Command::new("git")
        .args(["-C", &at, "rev-list", "--count", "HEAD"])
        .output()
        .expect("git failed to start");
    let count = String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .unwrap_or(0);
    (shallow, count)
}

fn checkout_in(scratch: &Path) -> PathBuf {
    scratch.join("deep/nest/checkout")
}

/// The two `--var`s the fixture reads. `None` leaves `reference` unset, which
/// is how the default-branch fixture is driven.
fn git_vars(remote: &Remote, reference: Option<&str>) -> Vec<String> {
    let mut v = vec!["--var".to_string(), format!("repo={}", remote.url())];
    if let Some(r) = reference {
        v.push("--var".to_string());
        v.push(format!("reference={r}"));
    }
    v
}

fn as_args(v: &[String]) -> Vec<&str> {
    v.iter().map(String::as_str).collect()
}

#[test]
fn git_clones_at_a_lightweight_tag_then_reads_ok() {
    let remote = Remote::new();
    let dir = tempfile::tempdir().unwrap();
    let args = git_vars(&remote, Some("light"));
    let args = as_args(&args);

    let (code, first) = apply(dir.path(), "git.yml", &args);
    assert_eq!(code, 0, "{first}");
    assert!(
        line_for(&first, "The checkout").contains("changed"),
        "{first}"
    );
    // The parents were two levels deep and did not exist.
    assert!(checkout_in(dir.path()).join(".git").is_dir(), "no checkout");

    let (code, second) = apply(dir.path(), "git.yml", &args);
    assert_eq!(code, 0, "{second}");
    assert_eq!(verdict(&second, "The checkout"), "ok", "{second}");
}

#[test]
fn an_annotated_tag_is_compared_by_the_commit_it_peels_to() {
    let remote = Remote::new();
    let dir = tempfile::tempdir().unwrap();
    let args = git_vars(&remote, Some("annotated"));
    let args = as_args(&args);

    let (code, first) = apply(dir.path(), "git.yml", &args);
    assert_eq!(code, 0, "{first}");
    assert!(
        line_for(&first, "The checkout").contains("changed"),
        "{first}"
    );

    // Without the `^{commit}` peel the tag's ref is the tag object's sha,
    // HEAD is the commit's, and this second run reports `changed` forever.
    let (code, second) = apply(dir.path(), "git.yml", &args);
    assert_eq!(code, 0, "{second}");
    assert_eq!(verdict(&second, "The checkout"), "ok", "{second}");
}

#[test]
fn a_pinned_ref_converges_with_no_remote_to_ask() {
    let remote = Remote::new();
    let dir = tempfile::tempdir().unwrap();
    let args = git_vars(&remote, Some("annotated"));
    let args = as_args(&args);
    let (code, _) = apply(dir.path(), "git.yml", &args);
    assert_eq!(code, 0);

    // §6.8: a tag is immutable, so once it is on disk the answer needs no
    // network. Deleting the remote outright is a harder test than pulling a
    // cable: every git call that reaches for it fails immediately.
    std::fs::remove_dir_all(remote.path()).unwrap();
    let (code, offline) = apply(dir.path(), "git.yml", &args);
    assert_eq!(code, 0, "{offline}");
    assert_eq!(verdict(&offline, "The checkout"), "ok", "{offline}");
}

#[test]
fn a_branch_fast_forwards_when_the_remote_moves() {
    let remote = Remote::new();
    let dir = tempfile::tempdir().unwrap();
    let args = git_vars(&remote, Some("main"));
    let args = as_args(&args);

    let (code, first) = apply(dir.path(), "git.yml", &args);
    assert_eq!(code, 0, "{first}");
    assert!(
        line_for(&first, "The checkout").contains("changed"),
        "{first}"
    );
    let at_clone = head_of(&checkout_in(dir.path()));

    let (code, second) = apply(dir.path(), "git.yml", &args);
    assert_eq!(code, 0, "{second}");
    assert_eq!(verdict(&second, "The checkout"), "ok", "{second}");

    remote.advance();
    let (code, moved) = apply(dir.path(), "git.yml", &args);
    assert_eq!(code, 0, "{moved}");
    assert!(
        line_for(&moved, "The checkout").contains("changed"),
        "{moved}"
    );
    assert_ne!(head_of(&checkout_in(dir.path())), at_clone, "did not move");

    let (code, settled) = apply(dir.path(), "git.yml", &args);
    assert_eq!(code, 0, "{settled}");
    assert_eq!(verdict(&settled, "The checkout"), "ok", "{settled}");
}

#[test]
fn a_branch_is_unknown_under_plan_and_a_missing_tag_with_it() {
    let remote = Remote::new();
    let dir = tempfile::tempdir().unwrap();
    let branch = git_vars(&remote, Some("main"));
    let branch = as_args(&branch);
    let (code, _) = apply(dir.path(), "git.yml", &branch);
    assert_eq!(code, 0);

    // §6.8: whether the remote moved is not a question the local clone
    // answers, and plan does not go to the network to ask.
    let path = fixture("git.yml");
    let mut argv = vec!["plan", path.to_str().unwrap()];
    argv.extend_from_slice(&branch);
    let out = run_in(dir.path(), &argv);
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    assert!(
        line_for(&text, "The checkout").contains("unknown"),
        "{text}"
    );

    // A tag the checkout has never heard of is the same answer for the same
    // reason: resolving it is a network call.
    remote.tag_later("v9", "light");
    let later = git_vars(&remote, Some("v9"));
    let mut argv = vec!["plan", path.to_str().unwrap()];
    argv.extend(as_args(&later));
    let out = run_in(dir.path(), &argv);
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    assert!(
        line_for(&text, "The checkout").contains("unknown"),
        "{text}"
    );

    // Apply fetches and then converges on it, which means moving HEAD back
    // to the commit the new tag names.
    let before = head_of(&checkout_in(dir.path()));
    let later = as_args(&later);
    let (code, got) = apply(dir.path(), "git.yml", &later);
    assert_eq!(code, 0, "{got}");
    assert!(line_for(&got, "The checkout").contains("changed"), "{got}");
    assert_ne!(
        head_of(&checkout_in(dir.path())),
        before,
        "HEAD did not move"
    );

    let (code, again) = apply(dir.path(), "git.yml", &later);
    assert_eq!(code, 0, "{again}");
    assert_eq!(verdict(&again, "The checkout"), "ok", "{again}");
}

#[test]
fn a_dirty_checkout_fails_and_is_not_reset() {
    let remote = Remote::new();
    let dir = tempfile::tempdir().unwrap();
    let light = git_vars(&remote, Some("light"));
    let light = as_args(&light);
    let (code, _) = apply(dir.path(), "git.yml", &light);
    assert_eq!(code, 0);

    let tracked = checkout_in(dir.path()).join("file.txt");
    std::fs::write(&tracked, "edited by hand\n").unwrap();
    let untracked = checkout_in(dir.path()).join("notes.txt");
    std::fs::write(&untracked, "mine\n").unwrap();

    let (code, out) = apply(dir.path(), "git.yml", &light);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("uncommitted changes"), "{out}");
    assert_eq!(
        std::fs::read_to_string(&tracked).unwrap(),
        "edited by hand\n",
        "the edit was discarded"
    );

    // §6.8 as amended: untracked files are not work a checkout can destroy,
    // so on their own they do not fail the step.
    std::fs::write(&tracked, "one\n").unwrap();
    let (code, clean) = apply(dir.path(), "git.yml", &light);
    assert_eq!(code, 0, "{clean}");
    assert!(untracked.is_file(), "the untracked file was removed");
}

#[test]
fn a_dest_whose_origin_is_another_repository_fails() {
    let remote = Remote::new();
    let other = Remote::new();
    let dir = tempfile::tempdir().unwrap();
    let mine = git_vars(&remote, Some("light"));
    let mine = as_args(&mine);
    let (code, _) = apply(dir.path(), "git.yml", &mine);
    assert_eq!(code, 0);

    let theirs = git_vars(&other, Some("light"));
    let theirs = as_args(&theirs);
    let (code, out) = apply(dir.path(), "git.yml", &theirs);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("origin"), "{out}");
}

#[test]
fn a_dest_that_is_not_a_repository_fails_rather_than_being_replaced() {
    let remote = Remote::new();
    let dir = tempfile::tempdir().unwrap();
    let target = checkout_in(dir.path());
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join("keep.txt"), "not mine to delete\n").unwrap();

    let args = git_vars(&remote, Some("light"));
    let args = as_args(&args);
    let (code, out) = apply(dir.path(), "git.yml", &args);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("not a git repository"), "{out}");
    assert!(target.join("keep.txt").is_file(), "the directory was taken");
}

#[test]
fn no_ref_takes_the_remotes_default_branch() {
    let remote = Remote::new();
    let dir = tempfile::tempdir().unwrap();
    let args = git_vars(&remote, None);
    let args = as_args(&args);

    let (code, first) = apply(dir.path(), "git_default_branch.yml", &args);
    assert_eq!(code, 0, "{first}");
    assert!(
        line_for(&first, "The checkout").contains("changed"),
        "{first}"
    );

    let (code, second) = apply(dir.path(), "git_default_branch.yml", &args);
    assert_eq!(code, 0, "{second}");
    assert_eq!(verdict(&second, "The checkout"), "ok", "{second}");

    remote.advance();
    let (code, moved) = apply(dir.path(), "git_default_branch.yml", &args);
    assert_eq!(code, 0, "{moved}");
    assert!(
        line_for(&moved, "The checkout").contains("changed"),
        "{moved}"
    );
}

impl Remote {
    /// Advance `main` and force-move an existing tag onto the new tip — what a
    /// project with a rolling `nightly` tag does every day.
    fn move_tag(&self, name: &str) {
        let work = self.root().join("retag");
        self.clone_into(&work);
        commit(&work, "retagged");
        git_at(&work, &["push", "--quiet", "origin", "main"]);
        git_at(&work, &["tag", "-f", name]);
        let tag = format!("refs/tags/{name}");
        git_at(&work, &["push", "--quiet", "--force", "origin", &tag]);
        discard(&work);
    }

    /// A branch named after the one path the history tracks, so a bare
    /// `git checkout <name>` cannot tell the branch from the file.
    fn branch_named_like_a_path(&self) -> &'static str {
        let work = self.root().join("branching");
        self.clone_into(&work);
        git_at(&work, &["checkout", "--quiet", "-b", "file.txt"]);
        commit(&work, "on a branch named like a path");
        git_at(&work, &["push", "--quiet", "origin", "file.txt"]);
        discard(&work);
        "file.txt"
    }
}

// Spec §6.8: a branch fetches on every apply, and `fetch --tags` without
// `--force` refuses to move a tag the checkout already has ("would clobber
// existing tag"). One tag moved upstream broke every branch step on that
// repository from then on.
#[test]
fn a_tag_moved_upstream_does_not_break_a_branch_checkout() {
    let remote = Remote::new();
    let dir = tempfile::tempdir().unwrap();
    let args = git_vars(&remote, Some("main"));
    let args = as_args(&args);

    let (code, first) = apply(dir.path(), "git.yml", &args);
    assert_eq!(code, 0, "{first}");

    remote.move_tag("light");
    let (code, moved) = apply(dir.path(), "git.yml", &args);
    assert_eq!(code, 0, "{moved}");
    assert!(
        line_for(&moved, "The checkout").contains("changed"),
        "{moved}"
    );
}

// A bare `git checkout <name>` with a tracked path of the same name is
// ambiguous, and git refuses rather than guess. The ref is a revision, and
// the checkout says so.
#[test]
fn a_ref_named_like_a_tracked_path_checks_out() {
    let remote = Remote::new();
    let name = remote.branch_named_like_a_path();
    let dir = tempfile::tempdir().unwrap();
    let args = git_vars(&remote, Some(name));
    let args = as_args(&args);

    let (code, first) = apply(dir.path(), "git.yml", &args);
    assert_eq!(code, 0, "{first}");
    assert!(
        line_for(&first, "The checkout").contains("changed"),
        "{first}"
    );

    let (code, second) = apply(dir.path(), "git.yml", &args);
    assert_eq!(code, 0, "{second}");
    assert_eq!(verdict(&second, "The checkout"), "ok", "{second}");
}

// ── `defaults` (spec §6.10) ───────────────────────────────────────────────

/// The non-macOS case, and now gated to say so.
///
/// The name always claimed this, but nothing enforced it, so on a Mac the
/// assertions failed — and the `apply` at the bottom is a real one.
/// `defaults.yml` names `com.apple.finder`, `com.apple.screencapture` and
/// `NSGlobalDomain`, so running the suite on a Mac converged the preferences
/// of whoever ran it: Finder's hidden files, the screenshot location and the
/// trackpad corner-click behaviour. The Mac side is
/// `defaults_applies_twice_changed_then_ok`, against a scratch plist.
#[cfg(not(target_os = "macos"))]
#[test]
fn defaults_off_macos_is_unknown_at_plan_and_a_failure_at_apply() {
    let dir = tempfile::tempdir().unwrap();
    let path = fixture("defaults.yml");

    // §6.10: the action does not skip itself. Plan cannot claim the keys are
    // set and does not pretend they are absent either.
    let out = run_in(dir.path(), &["plan", path.to_str().unwrap()]);
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    for step in [
        "Show hidden files",
        "Screenshots to the desktop",
        "Corner secondary click",
    ] {
        assert!(line_for(&text, step).contains("unknown"), "{text}");
    }

    // Apply says so instead of reporting a green line that did nothing.
    let (code, got) = apply(dir.path(), "defaults.yml", &[]);
    assert_eq!(code, 1, "{got}");
    assert!(got.contains("macOS only"), "{got}");
}

/// The macOS side of §6.10, which the suite had no coverage of: the only test
/// that applied `defaults` asserted the *failure* path, so on the one platform
/// where the action does something, nothing checked that it did it twice the
/// same way.
///
/// The domain is a plist inside the scratch directory. `defaults` takes an
/// absolute path wherever it takes a domain, so the action can be exercised
/// for real without touching the preferences of whoever is running this.
#[cfg(target_os = "macos")]
#[test]
fn defaults_applies_twice_changed_then_ok() {
    let dir = tempfile::tempdir().unwrap();

    let (code, first) = apply(dir.path(), "defaults_scratch.yml", &[]);
    assert_eq!(code, 0, "{first}");
    for step in ["A bool", "A string", "An int"] {
        assert!(line_for(&first, step).contains("changed"), "{first}");
    }

    // Spec §6: the second run is the one that matters.
    let (code, second) = apply(dir.path(), "defaults_scratch.yml", &[]);
    assert_eq!(code, 0, "{second}");
    for step in ["A bool", "A string", "An int"] {
        assert_eq!(verdict(&second, step), "ok", "{second}");
    }

    // Written where it was told to, and nowhere else.
    assert!(
        dir.path().join("provision-test.plist").is_file(),
        "the scratch plist was not created — did the domain resolve?"
    );
}

#[test]
fn a_no_probe_plan_lists_one_line_per_defaults_key() {
    let dir = tempfile::tempdir().unwrap();
    let path = fixture("defaults.yml");
    let out = run_in(
        dir.path(),
        &["plan", "--plan-no-probe", path.to_str().unwrap()],
    );
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    // One line per key, counted on the step lines rather than in the whole
    // output: the summary carries the same words and would double it.
    let steps = text
        .lines()
        .filter(|l| l.trim_start().starts_with('?'))
        .count();
    assert_eq!(steps, 3, "{text}");
    assert!(text.contains("3 steps · 3 would run (unprobed)"), "{text}");
}

// ── `download` (spec §6.9) ────────────────────────────────────────────────

/// The bytes every download test serves, and their published digest. Written
/// out rather than computed so a wrong digest encoding fails here instead of
/// looking like a bad download.
const BODY: &[u8] = b"provision\n";
const BODY_SHA: &str = "9afbc07eff35a28137e045624516e27201e86b89d3727b6ebecf0e6b49f65b0e";
/// The digest of something else, for the mismatch case.
const OTHER_SHA: &str = "a1621be95040239ee14362c16e20510ddc20f527d772d823b2a1679b33f5cd74";

/// A local HTTP listener, so the 404 case is a real non-2xx response rather
/// than a stand-in. Thirty lines of `std::net` beats a dependency or a
/// `python3 -m http.server` the suite would then need on every machine.
///
/// `GET /file` is the body; anything else is 404. The thread runs until the
/// test process exits.
fn http_server() -> String {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind a local listener");
    let port = listener
        .local_addr()
        .expect("the port the OS handed out")
        .port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { continue };
            let mut buf = [0u8; 1024];
            let n = s.read(&mut buf).unwrap_or(0);
            let head = String::from_utf8_lossy(buf.get(..n).unwrap_or(&[])).into_owned();
            let reply: Vec<u8> = if head.starts_with("GET /file ") {
                let mut v = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    BODY.len()
                )
                .into_bytes();
                v.extend_from_slice(BODY);
                v
            } else {
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()
            };
            // A client that hung up mid-reply is the 404 test's own doing:
            // curl closes as soon as it has what it needs.
            #[expect(
                clippy::let_underscore_must_use,
                reason = "nothing to do if the client hung up"
            )]
            let _ = s.write_all(&reply);
            #[expect(
                clippy::let_underscore_must_use,
                reason = "nothing to do if the client hung up"
            )]
            let _ = s.flush();
        }
    });
    format!("http://127.0.0.1:{port}")
}

/// A `file://` URL for a source file the test wrote.
fn file_url(path: &Path) -> String {
    format!("file://{}", path.display())
}

fn dl_vars(url: &str, sha: &str) -> Vec<String> {
    vec![
        "--var".to_string(),
        format!("url={url}"),
        "--var".to_string(),
        format!("sha={sha}"),
    ]
}

fn artifact_in(scratch: &Path) -> PathBuf {
    scratch.join("deep/nest/artifact.bin")
}

// Spec §6.9 takes `mode` "as `file` takes it", and §6.3 keeps an existing
// target's mode unless `mode` says otherwise. A re-fetch placed the new file
// at a flat 0644, so updating a tool to a new `sha256` with no `mode` left an
// executable nobody could run.
#[test]
fn a_download_that_replaces_a_file_keeps_its_mode() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.bin");
    std::fs::write(&source, BODY).unwrap();
    let tool = dir.path().join("bin/tool");
    std::fs::create_dir_all(dir.path().join("bin")).unwrap();
    std::fs::write(&tool, b"the old version\n").unwrap();
    std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
    let args = dl_vars(&file_url(&source), BODY_SHA);
    let args = as_args(&args);

    let (code, out) = apply(dir.path(), "download_keeps_mode.yml", &args);
    assert_eq!(code, 0, "{out}");
    assert!(line_for(&out, "The tool").contains("changed"), "{out}");
    assert_eq!(std::fs::read(&tool).unwrap(), BODY, "not replaced");
    let mode = std::fs::metadata(&tool).unwrap().permissions().mode() & 0o7777;
    assert_eq!(mode, 0o755, "the re-fetch reset the mode to {mode:o}");
}

/// Apply a fixture under `umask 077`, the one setting that tells a directory
/// created at 0755 from one created at whatever the umask leaves.
fn apply_under_umask_077(scratch: &Path, plan: &str, extra: &[&str]) -> (i32, String) {
    let path = fixture(plan);
    let out = Command::new("sh")
        .args(["-c", "umask 077; exec \"$0\" \"$@\""])
        .arg(env!("CARGO_BIN_EXE_provision"))
        .args(["apply", path.to_str().expect("fixture paths are UTF-8")])
        .args(extra)
        .env("PROVISION_SCRATCH", scratch)
        .env("NO_COLOR", "1")
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")))
        .output()
        .expect("sh failed to start");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
    )
}

fn dir_mode(p: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p)
        .expect("the directory the step created")
        .permissions()
        .mode()
        & 0o7777
}

// Spec §6.9 and §6.8: parents of `dest` are created the way `file` creates
// them, and §6.3 makes that 0755 whatever the umask. `download` and `git`
// used `create_dir_all`, so under `umask 077` the parents came out 0700 —
// a directory tree no other user could enter, unlike the same tree made by a
// `file` step.
#[test]
fn download_creates_parents_at_0755_under_any_umask() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.bin");
    std::fs::write(&source, BODY).unwrap();
    let args = dl_vars(&file_url(&source), BODY_SHA);
    let args = as_args(&args);

    let (code, out) = apply_under_umask_077(dir.path(), "download.yml", &args);
    assert_eq!(code, 0, "{out}");
    for d in ["deep", "deep/nest"] {
        let mode = dir_mode(&dir.path().join(d));
        assert_eq!(mode, 0o755, "{d} is {mode:o}");
    }
}

#[test]
fn depth_takes_one_commit_at_a_sha_and_then_converges_offline() {
    // Issue #6: pinning a large repository at a sha fetched its whole
    // history. §6.8: `depth: N` fetches the pinned commit and nothing else.
    let remote = Remote::new();
    let dir = tempfile::tempdir().unwrap();
    // Deliberately not the tip, and named by sha: `clone --depth` cannot
    // fetch a bare commit at all, which is why the shallow path is
    // `init` + `fetch` rather than a clone.
    let sha = rev_of(&remote.path(), "main~1");
    assert_eq!(sha.len(), 40, "the test did not get a sha");
    let args = git_vars(&remote, Some(&sha));
    let args = as_args(&args);

    let (code, first) = apply(dir.path(), "git_depth.yml", &args);
    assert_eq!(code, 0, "{first}");
    assert!(
        line_for(&first, "The checkout").contains("changed"),
        "{first}"
    );
    let at = checkout_in(dir.path());
    assert_eq!(head_of(&at), sha, "HEAD is not the pinned commit");
    assert_eq!(
        shallow_depth(&at),
        (true, 1),
        "the checkout is not one commit deep"
    );

    let (code, second) = apply(dir.path(), "git_depth.yml", &args);
    assert_eq!(code, 0, "{second}");
    assert_eq!(verdict(&second, "The checkout"), "ok", "{second}");

    // §6.8: a pinned ref is immutable, so the answer needs no network once
    // it is on disk — a shallow checkout included.
    std::fs::remove_dir_all(remote.path()).unwrap();
    let (code, offline) = apply(dir.path(), "git_depth.yml", &args);
    assert_eq!(code, 0, "{offline}");
    assert_eq!(verdict(&offline, "The checkout"), "ok", "{offline}");
    assert_eq!(
        shallow_depth(&at),
        (true, 1),
        "a later run deepened the checkout"
    );
}

#[test]
fn depth_at_an_annotated_tag_lands_the_tag_ref_too() {
    // The second run has to answer `ok` offline, and that answer goes
    // through `refs/tags/<ref>^{commit}` (§6.8). A shallow fetch of a bare
    // name leaves no tag ref behind; the refspec the shallow path uses does.
    let remote = Remote::new();
    let dir = tempfile::tempdir().unwrap();
    let args = git_vars(&remote, Some("annotated"));
    let args = as_args(&args);

    let (code, first) = apply(dir.path(), "git_depth.yml", &args);
    assert_eq!(code, 0, "{first}");
    let at = checkout_in(dir.path());
    assert_eq!(
        head_of(&at),
        rev_of(&remote.path(), "annotated^{commit}"),
        "HEAD is the tag object, not the commit it peels to"
    );
    assert_eq!(shallow_depth(&at), (true, 1));

    std::fs::remove_dir_all(remote.path()).unwrap();
    let (code, offline) = apply(dir.path(), "git_depth.yml", &args);
    assert_eq!(code, 0, "{offline}");
    assert_eq!(verdict(&offline, "The checkout"), "ok", "{offline}");
}

#[test]
fn depth_refuses_a_branch_and_writes_nothing() {
    // §6.8: a branch moves, and a shallow history has nothing to
    // fast-forward from. The remote is asked before anything is created, so
    // a plan that names one does not leave half a checkout behind.
    let remote = Remote::new();
    let dir = tempfile::tempdir().unwrap();
    let args = git_vars(&remote, Some("main"));
    let args = as_args(&args);

    let (code, out) = apply(dir.path(), "git_depth.yml", &args);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("main is a branch in"), "{out}");
    assert!(
        !checkout_in(dir.path()).exists(),
        "a refused step left a checkout behind:\n{out}"
    );
    // The parents of `dest` count: §6.8 says before anything is written.
    assert!(
        !dir.path().join("deep").exists(),
        "a refused step left its parent directories behind:\n{out}"
    );
}

#[test]
fn depth_without_a_ref_is_a_validation_error() {
    // No ref is the remote's default branch, and that is knowable without
    // asking anyone (§6.8).
    let dir = tempfile::tempdir().unwrap();
    let plan = dir.path().join("depth.yml");
    std::fs::write(
        &plan,
        "- name: Shallow at nothing in particular\n  git:\n    repo: https://example.invalid/r.git\n    dest: /tmp/r\n    depth: 1\n",
    )
    .unwrap();
    let out = run_in(dir.path(), &["validate", plan.to_str().unwrap()]);
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(3), "{text}");
    assert!(text.contains("`depth` requires a `ref`"), "{text}");
}

#[test]
fn git_creates_parents_at_0755_under_any_umask() {
    let remote = Remote::new();
    let dir = tempfile::tempdir().unwrap();
    let args = git_vars(&remote, Some("light"));
    let args = as_args(&args);

    let (code, out) = apply_under_umask_077(dir.path(), "git.yml", &args);
    assert_eq!(code, 0, "{out}");
    for d in ["deep", "deep/nest"] {
        let mode = dir_mode(&dir.path().join(d));
        assert_eq!(mode, 0o755, "{d} is {mode:o}");
    }
}

#[test]
fn download_verifies_the_hash_then_never_fetches_again() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.bin");
    std::fs::write(&source, BODY).unwrap();
    let args = dl_vars(&file_url(&source), BODY_SHA);
    let args = as_args(&args);

    // Plan never fetches (§6.9) but says what it would do.
    let path = fixture("download.yml");
    let mut argv = vec!["plan", path.to_str().unwrap()];
    argv.extend_from_slice(&args);
    let out = run_in(dir.path(), &argv);
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    assert!(
        line_for(&text, "The artifact").contains("would change"),
        "{text}"
    );
    assert!(!artifact_in(dir.path()).exists(), "plan fetched something");

    let (code, first) = apply(dir.path(), "download.yml", &args);
    assert_eq!(code, 0, "{first}");
    assert!(
        line_for(&first, "The artifact").contains("changed"),
        "{first}"
    );
    let got = artifact_in(dir.path());
    assert_eq!(std::fs::read(&got).unwrap(), BODY, "wrong content");
    // `mode` and the parents are `file`'s rules (§6.3).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&got).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "mode was not applied");
    }

    // §6.9: a matching hash is `ok` with no network. Removing the source
    // proves the second run never reached for it.
    std::fs::remove_file(&source).unwrap();
    let (code, second) = apply(dir.path(), "download.yml", &args);
    assert_eq!(code, 0, "{second}");
    assert_eq!(verdict(&second, "The artifact"), "ok", "{second}");
}

#[test]
fn a_hash_that_does_not_match_fails_naming_both_and_leaves_dest_alone() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.bin");
    std::fs::write(&source, BODY).unwrap();

    // Something is already there, and it must survive a failed fetch.
    let dest = artifact_in(dir.path());
    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
    std::fs::write(&dest, b"the file that was already here\n").unwrap();

    let args = dl_vars(&file_url(&source), OTHER_SHA);
    let args = as_args(&args);
    let (code, out) = apply(dir.path(), "download.yml", &args);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("is not the declared file"), "{out}");
    assert!(
        out.contains(OTHER_SHA),
        "the declared hash is not named:\n{out}"
    );
    assert!(
        out.contains(BODY_SHA),
        "the received hash is not named:\n{out}"
    );
    assert_eq!(
        std::fs::read(&dest).unwrap(),
        b"the file that was already here\n",
        "dest was replaced by a file that failed its check"
    );
}

#[test]
fn a_non_2xx_response_fails_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let base = http_server();

    let ok = dl_vars(&format!("{base}/file"), BODY_SHA);
    let ok = as_args(&ok);
    let (code, served) = apply(dir.path(), "download.yml", &ok);
    assert_eq!(code, 0, "{served}");
    assert_eq!(std::fs::read(artifact_in(dir.path())).unwrap(), BODY);

    std::fs::remove_file(artifact_in(dir.path())).unwrap();
    let missing = dl_vars(&format!("{base}/missing"), BODY_SHA);
    let missing = as_args(&missing);
    let (code, out) = apply(dir.path(), "download.yml", &missing);
    assert_eq!(code, 1, "{out}");
    assert!(
        !artifact_in(dir.path()).exists(),
        "a 404 left a file behind"
    );
}

#[test]
fn without_a_sha256_an_existing_dest_is_ok_and_is_never_fetched() {
    let dir = tempfile::tempdir().unwrap();
    // A URL that cannot resolve, so any fetch at all fails the step.
    let args = vec![
        "--var".to_string(),
        "url=https://nowhere.invalid/thing".to_string(),
    ];
    let args = as_args(&args);

    let (code, out) = apply(dir.path(), "download_nohash.yml", &args);
    assert_eq!(
        code, 1,
        "a missing dest with no hash must still fetch:\n{out}"
    );

    std::fs::write(dir.path().join("artifact.bin"), b"whatever\n").unwrap();
    let (code, second) = apply(dir.path(), "download_nohash.yml", &args);
    assert_eq!(code, 0, "{second}");
    assert_eq!(verdict(&second, "The artifact"), "ok", "{second}");
}

// #30: one line before the first step saying how much is about to happen.
// Free, because apply's preflight (§7) already visits every step to find the
// sudo ones. It reports *shape*, never a diff: `plan` is best-effort by D11
// and explicitly unprobed for a register-gated step by D14, so a promise
// about what would change, made before the run, is one provision cannot
// keep.
#[test]
fn apply_prints_the_shape_of_the_run_before_the_first_step() {
    let dir = tempfile::tempdir().unwrap();
    let plan = dir.path().join("plan.yml");
    std::fs::write(
        &plan,
        "description: a task\nsteps:\n  \
         - name: one\n    shell: \"true\"\n  \
         - name: two\n    shell: \"true\"\n  \
         - name: three\n    shell: \"true\"\n",
    )
    .unwrap();

    let out = run_in(dir.path(), &["apply", &plan.display().to_string()]);
    let text = String::from_utf8_lossy(&out.stdout);
    let first = text.lines().next().unwrap_or_default();
    assert!(first.contains("3 steps"), "{text}");
    // It is the root file's header, not a second line repeating the name.
    assert_eq!(
        text.matches("plan.yml").count(),
        2,
        "once at the top, once in the summary: {text}"
    );
    // No claim about what would change.
    assert!(!first.contains("would"), "{first}");
}

// On the stream too, as its own event (D25). A runner reading the stream
// learns the size of the job before the first step's result arrives.
#[test]
fn the_manifest_is_an_event_before_any_step_event() {
    let dir = tempfile::tempdir().unwrap();
    let plan = dir.path().join("plan.yml");
    std::fs::write(
        &plan,
        "description: a task\nsteps:\n  - name: one\n    shell: \"true\"\n",
    )
    .unwrap();

    let out = run_in(
        dir.path(),
        &["apply", "--json", &plan.display().to_string()],
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let first: serde_json::Value =
        serde_json::from_str(stdout.lines().next().unwrap()).expect("an event");
    assert_eq!(first["event"], "manifest");
    assert_eq!(first["steps"], 1);
    assert_eq!(first["files"], 1);
    assert_eq!(first["sudo"], 0);
}

/// §6.11: a target that already exists keeps the mode it has, and `mode`
/// sets one and is compared like §6.3's. The mode half of a `json` step is
/// `file`'s implementation and not a second one, so what this pins is the
/// delegation: a mode-only difference is `changed` with a metadata delta and
/// not a value diff, and a converged step is `ok` on both halves.
#[test]
fn a_json_step_converges_on_the_mode_as_well_as_the_keys() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("settings.json");
    std::fs::write(&target, "{\n  \"a\": 1\n}\n").unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();

    let plan = dir.path().join("p.yml");
    std::fs::write(
        &plan,
        format!(
            "description: a mode\nsteps:\n  - name: settings\n    json:\n      path: {}\n      content:\n        a: 1\n      mode: \"0644\"\n",
            target.display()
        ),
    )
    .unwrap();

    let out = run_in(dir.path(), &["apply", plan.to_str().unwrap()]);
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert_eq!(out.status.code(), Some(0), "{text}");
    assert_eq!(verdict(&text, "settings"), "changed", "{text}");
    // The values already agreed, so what changed is the mode — and the line
    // says so rather than showing a leaf that did not move.
    assert!(text.contains("mode 0600 → 0644"), "{text}");
    assert_eq!(
        std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o644
    );

    let out = run_in(dir.path(), &["apply", plan.to_str().unwrap()]);
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert_eq!(verdict(&text, "settings"), "ok", "{text}");
}
