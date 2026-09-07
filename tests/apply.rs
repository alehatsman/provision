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

// ── cmd, cwd, and the env sudo passes through ─────────────────────────────

#[test]
fn cmd_runs_an_argv_and_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();

    let (code, first) = apply(dir.path(), "cmd.yml", &[]);
    assert_eq!(code, 0, "{first}");
    assert!(line_for(&first, "spaces in it").contains("changed"), "{first}");
    assert!(line_for(&first, "built in vars").contains("changed"), "{first}");
    // No shell means no quoting: the space survives as one argument.
    assert!(dir.path().join("a dir with spaces").is_dir(), "{first}");
    assert!(dir.path().join("from-a-list").is_file(), "{first}");

    let (code, second) = apply(dir.path(), "cmd.yml", &[]);
    assert_eq!(code, 0, "{second}");
    assert!(!second.contains("changed"), "nothing should change twice:\n{second}");
    assert!(second.contains("2 skipped"), "{second}");
}

#[test]
fn cwd_defaults_to_the_plan_files_directory_and_can_be_overridden() {
    let dir = tempfile::tempdir().unwrap();
    let (code, out) = apply(dir.path(), "cwd_default.yml", &[]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(std::fs::read_to_string(dir.path().join("from-cwd-override")).unwrap(), "here");
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
    assert!(!out.contains("this must never run"), "the step ran anyway:\n{out}");
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
        .map(|o| o.status.success())
        .unwrap_or(false)
}


// ── file ──────────────────────────────────────────────────────────────────

#[test]
fn every_file_state_applies_twice_changed_then_ok() {
    let dir = tempfile::tempdir().unwrap();

    let (code, first) = apply(dir.path(), "file_states.yml", &[]);
    assert_eq!(code, 0, "{first}");
    for name in ["directory with an explicit mode", "inline content", "copied from src", "symlink"] {
        assert!(line_for(&first, name).contains("changed"), "{name}:\n{first}");
    }
    // Spec §6.3: `absent` on a path that was never there is ok, not changed.
    // This is the case the second run of an idempotency test lands on, and
    // here it is already true on the first.
    assert_eq!(verdict(&first, "never there"), "ok", "{first}");

    let conf = dir.path().join("conf");
    assert_eq!(mode_of(&conf), 0o700, "the explicit mode was not applied");
    assert_eq!(std::fs::read_to_string(conf.join("hello.txt")).unwrap(), "one\ntwo\n");
    assert_eq!(std::fs::read_to_string(conf.join("copied")).unwrap(), "copied from src\n");
    // Stored exactly as written: a relative link stays relative.
    assert_eq!(std::fs::read_link(conf.join("link")).unwrap().to_str().unwrap(), "./hello.txt");
    // But `~` is a path field the tool owns (spec §3), so it expands. Without
    // this the link never matches what is on disk and the step reports
    // changed on every run — which is how this was found, on a real plan.
    let home = std::env::var("HOME").unwrap();
    assert_eq!(
        std::fs::read_link(conf.join("home-link")).unwrap().to_str().unwrap(),
        format!("{home}/.bashrc")
    );

    let (code, second) = apply(dir.path(), "file_states.yml", &[]);
    assert_eq!(code, 0, "{second}");
    assert!(!second.contains("changed"), "nothing should change twice:\n{second}");
    assert!(second.contains("6 ok"), "{second}");
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
    assert!(out.contains("mode 0755 → 0700"), "the delta is not reported:\n{out}");
    assert_eq!(mode_of(&conf), 0o700);
}

#[test]
fn plan_shows_a_diff_and_no_diff_suppresses_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = fixture("file_states.yml");

    let out = run_in(dir.path(), &["plan", path.to_str().unwrap()]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(2), "{text}");
    assert!(line_for(&text, "inline content").contains("would change"), "{text}");
    assert!(text.contains("+one"), "the diff is missing:\n{text}");
    assert!(!dir.path().join("conf").exists(), "plan created something");

    let out = run_in(dir.path(), &["plan", path.to_str().unwrap(), "--no-diff"]);
    let quiet = String::from_utf8_lossy(&out.stdout);
    assert!(quiet.contains("would change"), "{quiet}");
    assert!(!quiet.contains("+one"), "--no-diff did not suppress it:\n{quiet}");
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
        assert!(Command::new("sudo")
            .args(["-n", "chown", "root:root", secret.to_str().unwrap()])
            .status()
            .map(|s| s.success())
            .unwrap_or(false));

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
        let _ = Command::new("sudo").args(["-n", "rm", "-f", secret.to_str().unwrap()]).status();

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
    assert!(sudo(&["chown", "root:root", &owned]) && sudo(&["chmod", "0755", &owned]));

    let run = |extra: &[&str]| {
        let path = fixture("file_sudo.yml");
        let mut args = vec!["apply", path.to_str().unwrap()];
        args.extend_from_slice(extra);
        let out = Command::new(env!("CARGO_BIN_EXE_provision"))
            .args(&args)
            .env("PROVISION_SCRATCH", dir.path())
            .env("PROVISION_ROOT_DIR", &owned)
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
    let stat = Command::new("sudo")
        .args(["-n", "stat", "-c%a %U %G", &format!("{owned}/provision.conf")])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    let _ = sudo(&["rm", "-rf", &owned]);

    assert_eq!(code, 0, "{first}");
    assert!(line_for(&first, "root-owned config").contains("changed"), "{first}");
    assert_eq!(stat, "440 root root", "install did not land the metadata");
    assert_eq!(code2, 0, "{second}");
    assert!(!second.contains("changed"), "the sudo path is not idempotent:\n{second}");
}

fn sudo(args: &[&str]) -> bool {
    let mut all = vec!["-n"];
    all.extend_from_slice(args);
    Command::new("sudo").args(&all).status().map(|s| s.success()).unwrap_or(false)
}

fn mode_of(p: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).unwrap().permissions().mode() & 0o7777
}

fn set_mode(p: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).unwrap();
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
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stdout));
    assert_eq!(mode_of(&dir.path().join("a")), 0o755, "parent took the umask");
    assert_eq!(mode_of(&dir.path().join("a/b")), 0o755, "parent took the umask");
    assert_eq!(mode_of(&dir.path().join("a/b/c")), 0o700, "the mode missed the leaf");
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
    assert!(text.contains("\"0644\""), "the note does not show the fix: {text}");
    assert!(text.contains("unquoted.yml:6:11"), "{text}");
}


// ── template ──────────────────────────────────────────────────────────────

#[test]
fn a_template_and_a_tree_apply_twice_changed_then_ok() {
    let dir = tempfile::tempdir().unwrap();

    let (code, first) = apply(dir.path(), "template.yml", &[]);
    assert_eq!(code, 0, "{first}");
    assert!(line_for(&first, "single template").contains("changed"), "{first}");
    // Spec §6.4: one step, one line, and the line carries the count.
    assert!(line_for(&first, "whole tree").contains("3 of 3"), "{first}");

    let out = dir.path().join("rendered");
    assert_eq!(std::fs::read_to_string(dir.path().join("one")).unwrap(), "single file for linux\n");
    // The `.j2` comes off the destination name; a file without one is still
    // rendered.
    assert!(out.join("top.conf").is_file(), "the .j2 suffix was not stripped");
    assert!(std::fs::read_to_string(out.join("nested/inner.conf")).unwrap().contains("linux"));
    // Spec §6.4: a file that is not valid UTF-8 is placed byte for byte.
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/apply/tree/nested/blob.bin");
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
    assert!(!second.contains("changed"), "nothing should change twice:\n{second}");
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
    assert!(!out.contains("inner.conf"), "an unchanged file was reported:\n{out}");
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
    path: std::path::PathBuf,
}

impl Unit {
    fn new() -> Option<Unit> {
        let runtime = std::env::var("XDG_RUNTIME_DIR").ok()?;
        let dir = Path::new(&runtime).join("systemd/user");
        std::fs::create_dir_all(&dir).ok()?;
        // Tests share one process, so the pid alone is not unique enough:
        // two of them would build the same unit name and each Drop would
        // remove the other's file.
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
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
            let _ = std::fs::remove_file(&path);
            return None;
        }
        Some(Unit { name, path })
    }
}

impl Drop for Unit {
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
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn service_plan(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
    let plan = dir.join("service.yml");
    std::fs::write(
        &plan,
        format!("- name: The test unit\n  service:\n    name: {name}\n    scope: user\n{body}"),
    )
    .unwrap();
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
        (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned())
    };

    let (code, first) = run("    state: started\n");
    assert_eq!(code, 0, "{first}");
    assert!(line_for(&first, "test unit").contains("changed"), "{first}");
    assert!(first.contains(&format!("start {}", unit.name)), "{first}");

    let (code, second) = run("    state: started\n");
    assert_eq!(code, 0, "{second}");
    assert_eq!(verdict(&second, "test unit"), "ok", "already started:\n{second}");

    // Spec §6.6: `restarted` has no before-state, so it is always changed.
    let (code, restarted) = run("    state: restarted\n");
    assert_eq!(code, 0, "{restarted}");
    assert!(line_for(&restarted, "test unit").contains("changed"), "{restarted}");

    let (code, stopped) = run("    state: stopped\n");
    assert_eq!(code, 0, "{stopped}");
    assert!(line_for(&stopped, "test unit").contains("changed"), "{stopped}");
    let (code, again) = run("    state: stopped\n");
    assert_eq!(code, 0, "{again}");
    assert_eq!(verdict(&again, "test unit"), "ok", "already stopped:\n{again}");
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
    assert_eq!(out.status.code(), Some(0), "a disabled unit is already so:\n{text}");
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
