//! The phase 1 gate: execution.
//!
//! Every action ships with a test that runs it twice on a scratch filesystem
//! and asserts `changed` then `ok` (spec §11). For `shell` that means the two
//! gates it can declare — `unless` and `creates` — and for `assert` it means
//! that running twice changes nothing at all.

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
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/apply").join(name)
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
    let mut args = vec!["apply", path.to_str().unwrap()];
    args.extend_from_slice(extra);
    let out = run_in(scratch, &args);
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned()
            + &String::from_utf8_lossy(&out.stderr),
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
    assert!(line_for(&first, "scratch directory").contains("changed"), "{first}");
    assert!(line_for(&first, "identity key").contains("changed"), "{first}");
    assert!(line_for(&first, "identity key present").contains("ok"), "{first}");
    assert!(dir.path().join(".ssh").is_dir(), "the directory was not made");
    assert!(dir.path().join("id_ed25519").is_file(), "the key was not written");

    let (code, second) = apply(dir.path(), "scratch_home.yml", &[]);
    assert_eq!(code, 0, "{second}");
    // `creates` and `unless` are the two ways a shell step declares that it is
    // already done. Both must fire on the second run and neither on the first.
    assert!(line_for(&second, "scratch directory").contains("creates exists"), "{second}");
    assert!(line_for(&second, "identity key").contains("unless"), "{second}");
    assert!(line_for(&second, "identity key present").contains("ok"), "{second}");
    assert!(!second.contains("changed"), "nothing should change twice:\n{second}");
}

#[test]
fn plan_probes_the_same_gates_apply_uses() {
    let dir = tempfile::tempdir().unwrap();
    let path = fixture("scratch_home.yml");
    let plan = |d: &Path| {
        let out = run_in(d, &["plan", path.to_str().unwrap()]);
        (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned())
    };

    // Nothing has run, so the closing assert cannot hold yet: exit 1. Spec
    // §6.7 — plan reports that and keeps walking, so all three steps are here.
    let (code, before) = plan(dir.path());
    assert_eq!(code, 1, "{before}");
    assert!(line_for(&before, "scratch directory").contains("would run"), "{before}");
    assert!(line_for(&before, "identity key present").contains("FAILED"), "{before}");
    assert!(before.contains("3 steps"), "{before}");
    assert!(!dir.path().join(".ssh").exists(), "plan must not create anything");

    apply(dir.path(), "scratch_home.yml", &[]);

    let (code, after) = plan(dir.path());
    assert_eq!(code, 0, "a converged plan has nothing to do:\n{after}");
    assert!(line_for(&after, "scratch directory").contains("creates exists"), "{after}");
}

// ── verdicts ──────────────────────────────────────────────────────────────

#[test]
fn each_modifier_reaches_its_own_verdict() {
    let dir = tempfile::tempdir().unwrap();
    let (code, out) = apply(dir.path(), "verdicts.yml", &[]);
    assert_eq!(code, 0, "{out}");

    // Spec §6.1: no gate, no verdict. `unknown` is an answer, not a failure.
    assert_eq!(verdict(&out, "ungated shell"), "unknown", "{out}");
    assert!(line_for(&out, "work is done").contains("unless"), "{out}");
    assert_eq!(verdict(&out, "changed_when decides"), "changed", "{out}");
    // The register held a real value, so `when` was answerable at apply time.
    assert_eq!(verdict(&out, "reads the register"), "ok", "{out}");
    // `failed_when` forgave exit 3, so the step is a success.
    assert_eq!(verdict(&out, "forgives a non-zero"), "ok", "{out}");
}

#[test]
fn an_unknown_verdict_makes_plan_exit_two() {
    // D15: a step provision cannot judge is not a step it may call converged.
    let dir = tempfile::tempdir().unwrap();
    let path = fixture("verdicts.yml");
    let out = run_in(dir.path(), &["plan", path.to_str().unwrap()]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(2), "{text}");
    assert_eq!(verdict(&text, "ungated shell"), "unknown", "{text}");
}

#[test]
fn plan_no_probe_claims_nothing_and_exits_zero() {
    let dir = tempfile::tempdir().unwrap();
    let path = fixture("verdicts.yml");
    let out = run_in(dir.path(), &["plan", "--plan-no-probe", path.to_str().unwrap()]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "{text}");
    assert!(text.contains("would run (unprobed)"), "{text}");
    assert!(!text.contains("unknown"), "nothing was probed, so nothing is unknown:\n{text}");
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
    assert!(!out.contains("Never reached"), "the run must stop at the failure:\n{out}");
}

#[test]
fn an_unimplemented_action_says_which_phase_brings_it() {
    let dir = tempfile::tempdir().unwrap();
    let (code, out) = apply(dir.path(), "not_yet.yml", &[]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("`file` is not implemented yet (phase 2)"), "{out}");
    // But it still plans, which is what keeps the dotfiles tree usable today.
    let path = fixture("not_yet.yml");
    let out = run_in(dir.path(), &["plan", "--plan-no-probe", path.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(0));
}

// ── retry and timeout ─────────────────────────────────────────────────────

#[test]
fn retry_reruns_until_the_step_passes() {
    let dir = tempfile::tempdir().unwrap();
    let (code, out) = apply(dir.path(), "retry.yml", &[]);
    assert_eq!(code, 0, "{out}");
    assert!(line_for(&out, "third attempt").contains("attempt 3/3"), "{out}");
    assert!(line_for(&out, "third attempt").contains("changed"), "{out}");
    assert_eq!(std::fs::read_to_string(dir.path().join("tries")).unwrap().trim(), "3");
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
        .map(|o| o.status.success())
        .unwrap_or(false);
    assert!(!alive, "pid {pid} outlived the process group it was killed with");
}

// ── output forms ──────────────────────────────────────────────────────────

#[test]
fn json_emits_one_object_per_step_and_a_summary() {
    let dir = tempfile::tempdir().unwrap();
    let path = fixture("verdicts.yml");
    let out = run_in(dir.path(), &["apply", path.to_str().unwrap(), "--json"]);
    let stdout = String::from_utf8_lossy(&out.stdout);

    let rows: Vec<serde_json::Value> =
        stdout.lines().map(|l| serde_json::from_str(l).expect(l)).collect();
    assert_eq!(rows.last().unwrap()["event"], "summary");

    let steps: Vec<&serde_json::Value> =
        rows.iter().filter(|r| r["event"] == "step").collect();
    assert_eq!(steps.len(), 5, "{stdout}");
    assert_eq!(steps[0]["status"], "unknown");
    assert_eq!(steps[1]["status"], "skipped");
    assert_eq!(steps[1]["reason"], "unless");
    assert_eq!(steps[2]["status"], "changed");
    // Spec §9.3: every step carries the file and line it came from.
    assert!(steps[0]["line"].as_u64().unwrap() > 0, "{stdout}");
    assert!(steps[0]["file"].as_str().unwrap().ends_with("verdicts.yml"), "{stdout}");

    // Human output moved to stderr so stdout stays parseable.
    assert!(String::from_utf8_lossy(&out.stderr).contains("ungated shell"));

    snapshot!("json", stdout);
}

#[test]
fn hide_skipped_drops_the_lines_but_not_the_count() {
    let dir = tempfile::tempdir().unwrap();
    let (code, out) = apply(dir.path(), "verdicts.yml", &["--hide-skipped"]);
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains("work is done"), "{out}");
    assert!(out.contains("1 skipped"), "the summary still counts it:\n{out}");
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

#[test]
fn snapshot_retry_rendering() {
    let dir = tempfile::tempdir().unwrap();
    let (_, out) = apply(dir.path(), "retry.yml", &[]);
    snapshot!("retry", out);
}

// ── context, streaming, and Ctrl-C ────────────────────────────────────────

#[test]
fn cwd_and_env_both_reach_the_step() {
    let dir = tempfile::tempdir().unwrap();
    let (code, out) = apply(dir.path(), "context.yml", &[]);
    assert_eq!(code, 0, "{out}");
    // Written by a relative path, so it landed only if `cwd` was honored.
    assert_eq!(std::fs::read_to_string(dir.path().join("from-cwd")).unwrap(), "hello");
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

#[test]
fn ctrl_c_kills_the_step_and_exits_130() {
    let dir = tempfile::tempdir().unwrap();
    let path = fixture("interrupt.yml");
    let child = Command::new(env!("CARGO_BIN_EXE_provision"))
        .args(["apply", path.to_str().unwrap()])
        .env("PROVISION_SCRATCH", dir.path())
        .env("NO_COLOR", "1")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    // Long enough for the step to be running, short enough to stay a test.
    std::thread::sleep(std::time::Duration::from_millis(600));
    let _ = Command::new("kill").args(["-INT", &child.id().to_string()]).status();

    let out = child.wait_with_output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned()
        + &String::from_utf8_lossy(&out.stderr);
    // Spec §10: the current step is killed, the summary is still printed, and
    // the shell's convention for SIGINT is the exit code.
    assert_eq!(out.status.code(), Some(130), "{text}");
    assert!(text.contains("interrupted"), "{text}");
    assert!(text.contains("1 step"), "the summary is still printed:\n{text}");
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
