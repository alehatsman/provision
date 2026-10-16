//! The apply engine, exercised on every platform provision supports.
//!
//! `tests/apply.rs` is `#![cfg(unix)]` for a good reason — most of it is
//! about modes, `chown`, `stat` and sudo, which are Unix behaviour and not
//! merely inconvenient to write portably. But the engine underneath it is
//! not: a verdict, a gate, a register, the JSON shape, the deadline and the
//! stop-on-failure rule are the model, and the model is the same everywhere.
//! None of that ran on Windows at all (#26), so a regression in any of it
//! was invisible there until someone ran the tool.
//!
//! Every plan here is written into the test's own tempdir rather than
//! committed as a fixture, and every command is one both `bash` and
//! `powershell` accept — `exit 0`, `echo hello` — so the assertions are
//! about provision and not about which shell read them. Where no single
//! spelling works, the helper branches on the same `cfg!` the code uses
//! (`sleep_for`), which is the rule `tests/apply.rs` opens by stating.

use std::path::Path;
use std::process::Command;

fn run(dir: &Path, args: &[&str]) -> (i32, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_provision"))
        .args(args)
        .env("NO_COLOR", "1")
        .current_dir(dir)
        .output()
        .expect("provision failed to start");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
    )
}

/// Write a plan and apply it. The plan is a task root (`description:` +
/// `steps:`), so the idempotency gate is opt-in here and a step is free to
/// be ungated when that is what is under test (D26).
fn apply(dir: &Path, body: &str, extra: &[&str]) -> (i32, String) {
    let path = dir.join("plan.yml");
    std::fs::write(
        &path,
        format!("description: a cross-platform plan\nsteps:\n{body}"),
    )
    .expect("writing into the test's own tempdir");
    let mut args = vec!["apply", "plan.yml"];
    args.extend_from_slice(extra);
    run(dir, &args)
}

/// Sleep, spelled for the shell this platform runs (§6.1: bash on unix,
/// powershell on Windows).
fn sleep_for(secs: u32) -> String {
    if cfg!(windows) {
        format!("Start-Sleep -Seconds {secs}")
    } else {
        format!("sleep {secs}")
    }
}

/// The status column for one step, by name.
///
/// A missing line is this helper's assertion failure; panicking with the
/// whole output is how the test says what it expected.
#[expect(clippy::panic, reason = "a test helper's assertion failure")]
fn verdict(out: &str, name: &str) -> String {
    let line = out
        .lines()
        .find(|l| l.contains(name))
        .unwrap_or_else(|| panic!("no line for `{name}` in:\n{out}"));
    let rest = line.split(name).nth(1).unwrap_or_default().trim();
    rest.rsplit_once(char::is_whitespace)
        .map_or(rest, |(label, _)| label)
        .trim()
        .to_string()
}

fn rows(stdout: &str) -> Vec<serde_json::Value> {
    stdout
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

// §9.1's vocabulary, which is the whole of what a reader and a runner get
// per step. A gate that says the work is done skips; `changed_when` decides
// when nothing else can; a non-zero exit fails.
#[test]
fn the_four_verdicts_read_the_same_on_every_platform() {
    let dir = tempfile::tempdir().unwrap();
    let (code, out) = apply(
        dir.path(),
        "  - name: plain ok\n    shell: \"exit 0\"\n    changed_when: \"false\"\n\
         \x20 - name: says changed\n    shell: \"exit 0\"\n    changed_when: \"true\"\n\
         \x20 - name: gated off\n    shell: \"exit 0\"\n    when: \"false\"\n\
         \x20 - name: forgiven\n    shell: \"exit 3\"\n    failed_when: \"false\"\n",
        &[],
    );
    assert_eq!(code, 0, "{out}");
    assert_eq!(verdict(&out, "plain ok"), "ok", "{out}");
    assert_eq!(verdict(&out, "says changed"), "changed", "{out}");
    assert_eq!(verdict(&out, "gated off"), "skipped   when: false", "{out}");
    assert_eq!(verdict(&out, "forgiven"), "ok", "{out}");
}

// §2: apply stops at the first failure. Nothing after it runs, and the run
// exits 1 — the half of the contract a CI runner reads first.
#[test]
fn a_failure_stops_the_run() {
    let dir = tempfile::tempdir().unwrap();
    let (code, out) = apply(
        dir.path(),
        "  - name: fails\n    shell: \"exit 2\"\n\
         \x20 - name: must not run\n    shell: \"exit 0\"\n    changed_when: \"false\"\n",
        &[],
    );
    assert_eq!(code, 1, "{out}");
    assert!(
        !out.contains("must not run"),
        "a step ran after a failure:\n{out}"
    );
}

// `--keep-going` carries on past one, and the exit code still says the run
// failed.
#[test]
fn keep_going_reaches_the_step_after_a_failure() {
    let dir = tempfile::tempdir().unwrap();
    let (code, out) = apply(
        dir.path(),
        "  - name: fails\n    shell: \"exit 2\"\n\
         \x20 - name: still runs\n    shell: \"exit 0\"\n    changed_when: \"false\"\n",
        &["--keep-going"],
    );
    assert_eq!(code, 1, "{out}");
    assert_eq!(verdict(&out, "still runs"), "ok", "{out}");
}

// §4: a register carries the command's own `rc` and `stdout` to a later
// step, which is the mechanism D5 put in handlers' place.
#[test]
fn a_later_step_reads_the_register() {
    let dir = tempfile::tempdir().unwrap();
    let (code, out) = apply(
        dir.path(),
        "  - name: produce\n    shell: \"echo hello\"\n    register: r\n    changed_when: \"false\"\n\
         \x20 - name: consume\n    shell: \"exit 0\"\n    when: \"r.rc == 0 and 'hello' in r.stdout\"\n    changed_when: \"false\"\n",
        &[],
    );
    assert_eq!(code, 0, "{out}");
    assert_eq!(verdict(&out, "consume"), "ok", "{out}");
}

// §9.3: one object per step, then one summary, and the words come from
// `Status::key` rather than the terminal wording. This is the contract D19
// gave a runner and D25 gave an agent.
#[test]
fn the_json_stream_has_the_same_shape_on_every_platform() {
    let dir = tempfile::tempdir().unwrap();
    let (code, out) = apply(
        dir.path(),
        "  - name: one\n    shell: \"exit 0\"\n    changed_when: \"false\"\n",
        &["--json"],
    );
    assert_eq!(code, 0, "{out}");
    let rows = rows(&out);
    let manifest = rows
        .iter()
        .find(|r| r["event"] == "manifest")
        .expect("a manifest");
    assert_eq!(manifest["steps"], 1, "{out}");
    let step = rows.iter().find(|r| r["event"] == "step").expect("a step");
    assert_eq!(step["status"], "ok", "{out}");
    assert_eq!(step["rc"], 0, "{out}");
    let summary = rows
        .iter()
        .find(|r| r["event"] == "summary")
        .expect("a summary");
    assert_eq!(summary["total"], 1, "{out}");
    assert_eq!(summary["ok"], 1, "{out}");
    assert_eq!(summary["interrupted"], false, "{out}");
    assert_eq!(summary["deadline_exceeded"], false, "{out}");
}

// §8: the run's clock is provision's to keep (D19). The step is killed, the
// run exits 124, and the step after it never starts.
#[test]
fn the_deadline_ends_the_run_and_exits_124() {
    let dir = tempfile::tempdir().unwrap();
    let (code, out) = apply(
        dir.path(),
        &format!(
            "  - name: too slow\n    shell: \"{}\"\n\
             \x20 - name: must not run\n    shell: \"exit 0\"\n    changed_when: \"false\"\n",
            sleep_for(30)
        ),
        &["--deadline", "2s"],
    );
    assert_eq!(code, 124, "{out}");
    assert!(out.contains("deadline exceeded"), "{out}");
    assert!(
        !out.contains("must not run"),
        "a step ran past the deadline:\n{out}"
    );
}

// §6.3: `file` is a typed action with no shell in it at all, so it is the
// same work on every platform — and the second apply is the one that proves
// it converges rather than merely runs.
#[test]
fn a_file_step_writes_once_and_then_is_ok() {
    let dir = tempfile::tempdir().unwrap();
    let body =
        "  - name: write it\n    file: { path: ./out.txt, state: file, content: \"hello\\n\" }\n";
    let (code, out) = apply(dir.path(), body, &[]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(verdict(&out, "write it"), "changed", "{out}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("out.txt")).unwrap(),
        "hello\n"
    );

    let (code, out) = apply(dir.path(), body, &[]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(
        verdict(&out, "write it"),
        "ok",
        "a second apply was not a no-op:\n{out}"
    );
}

// §6.3: "Changed when content, mode, owner, group, or type differs" — so a
// `file` step that declares a `mode` has to converge on the second apply the
// same way one that declares only content does. The sibling above applies
// twice without a `mode:`, which is the half that works everywhere.
//
// Ignored on Windows, where it fails: `mode_of` reads 0 for every target and
// `set_mode` writes nothing, so the delta is reported and never closed and
// the step is `changed` forever (#57). `tasks/install.yml:40` is exactly this
// step. The ignore comes off with whichever fix lands.
#[test]
#[cfg_attr(windows, ignore = "#57: mode is read as 0 and written nowhere")]
fn a_file_step_with_a_mode_converges() {
    let dir = tempfile::tempdir().unwrap();
    let body = "  - name: write it\n    file: { path: ./moded.txt, state: file, content: \"hello\\n\", mode: \"0640\" }\n";
    let (code, out) = apply(dir.path(), body, &[]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(verdict(&out, "write it"), "changed", "{out}");

    let (code, out) = apply(dir.path(), body, &[]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(
        verdict(&out, "write it"),
        "ok",
        "a `mode` that is already set was reported as a change:\n{out}"
    );
}

// §8: tag selection is filtering, not failure — the steps it excludes are
// reported skipped and the run is green.
#[test]
fn tags_select_which_steps_run() {
    let dir = tempfile::tempdir().unwrap();
    let (code, out) = apply(
        dir.path(),
        "  - name: tagged db\n    shell: \"exit 0\"\n    changed_when: \"false\"\n    tags: [db]\n\
         \x20 - name: tagged web\n    shell: \"exit 0\"\n    changed_when: \"false\"\n    tags: [web]\n",
        &["--tags", "db"],
    );
    assert_eq!(code, 0, "{out}");
    assert_eq!(verdict(&out, "tagged db"), "ok", "{out}");
    assert!(verdict(&out, "tagged web").starts_with("skipped"), "{out}");
}

/// A script that starts a grandchild which waits and then leaves a mark,
/// and then blocks forever itself.
///
/// The mark is the assertion: provision kills the step's whole tree, not
/// just the shell it spawned, so the grandchild must never get to write it.
/// On unix that is `killpg`; on Windows it is `taskkill /T`, and until this
/// test the Windows half was executed by nothing at all.
fn grandchild_marking(marker: &Path) -> String {
    let m = marker.display().to_string().replace('\\', "/");
    if cfg!(windows) {
        // `Start-Process` makes a real child of this shell, which is what
        // `taskkill /T` walks. A background job would not be one.
        format!(
            "Start-Process -NoNewWindow powershell -ArgumentList '-NoProfile','-Command',\
             'Start-Sleep -Seconds 5; Set-Content -Path \"{m}\" -Value mark'; \
             Start-Sleep -Seconds 60"
        )
    } else {
        format!("(sleep 5; echo mark > \"{m}\") &\nsleep 60\n")
    }
}

// Spec §4: `timeout` bounds the step, and a step is not its shell — a child
// the shell started is still the step's. Both platforms promise it and only
// one of them was tested: every kill and reap test lives in `tests/apply.rs`,
// which is `#![cfg(unix)]`, so `taskkill /T` (`src/exec/process.rs:458`) ran
// in no test on any machine. `tests/apply_cross_platform.rs` had a deadline
// test, but it asserts exit 124 and says nothing about what is still
// running afterwards — a kill that reached only the shell passes it.
#[test]
fn a_timeout_kills_the_whole_tree_on_every_platform() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("marker.txt");
    let started = std::time::Instant::now();
    let (code, out) = apply(
        dir.path(),
        &format!(
            "  - name: spawns a grandchild\n    shell: |\n{}\n    timeout: 2s\n    changed_when: \"false\"\n",
            grandchild_marking(&marker)
                .lines()
                .map(|l| format!("      {l}"))
                .collect::<Vec<_>>()
                .join("\n")
        ),
        &[],
    );
    let elapsed = started.elapsed();
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("timed out after"), "{out}");
    // The step's own clock, not the run's: nothing here passes `--deadline`.
    assert!(
        elapsed < std::time::Duration::from_secs(20),
        "the step outlived its own timeout: {elapsed:?}\n{out}"
    );
    // Past when the grandchild would have written, had anything let it.
    std::thread::sleep(std::time::Duration::from_secs(5));
    assert!(
        !marker.exists(),
        "a grandchild outlived the step it belonged to and wrote {}",
        marker.display()
    );
}
