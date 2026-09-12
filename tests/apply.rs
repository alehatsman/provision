//! The phase 1 gate: execution.
//!
//! Every action ships with a test that runs it twice on a scratch filesystem
//! and asserts `changed` then `ok` (spec §11). For `shell` that means the two
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
    assert!(
        out.contains("stdout+stderr, last 20 lines each"),
        "the note must name both streams:\n{out}"
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
        out.contains("stderr, last 20 lines"),
        "one stream, so the note reads as it always did:\n{out}"
    );
    assert!(
        !out.contains("stdout+stderr"),
        "there was no stdout to name:\n{out}"
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
// `delay` at a time even though no single attempt ever ran long. Without a
// recheck before each attempt, the loop would keep going on the stale
// timeout computed once at prepare time and blow well past the deadline
// (attempts: 20, delay: 1s — twenty seconds of retrying against a
// one-second deadline).
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
    // The regression this guards: without a per-attempt recheck the loop
    // keeps retrying on the stale prepare-time timeout and only stops
    // between steps, burning all 20 attempts * 1s delay (~20s) against a
    // 1s deadline before anything reports it. The between-step deadline
    // check alone still yields exit 124, so only wall-clock time tells the
    // two apart.
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "the retry ran past the deadline instead of stopping between attempts: {elapsed:?}"
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
    let at = dir.display().to_string();
    let out = Command::new("git")
        .args(["-C", &at, "rev-parse", "HEAD"])
        .output()
        .expect("git failed to start");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
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

    // Spec §11: the second run is the one that matters.
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
