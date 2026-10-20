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
// It used to fail on Windows: `mode_of` reads 0 for every target and
// `set_mode` writes nothing, so the delta was reported and never closed and
// the step was `changed` forever (#57) — on `tasks/install.yml:40`, which is
// exactly this step. A platform with no modes now reports none, so the step
// converges there on the second apply like everywhere else.
#[test]
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
        // `&&`, not `;`: a TERM that kills the `sleep` leaves its exit
        // status non-zero and the write is skipped. With `;` the shell runs
        // the next command before acting on its own pending TERM, so the
        // mark appeared at the kill rather than five seconds later -- a
        // flake in the test, not in the kill, and one that reported the
        // opposite of what happened.
        format!("(sleep 5 && echo mark > \"{m}\") &\nsleep 60\n")
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

// Spec §10's Ctrl-C row, on the platform where it was never true: with no
// console handler installed, Windows ended provision on the spot — exit
// 0xC000013A, no summary, no `interrupted`. `tests/apply.rs` has the unix
// twin, `ctrl_c_kills_the_step_and_exits_130`, and it is `#![cfg(unix)]`.
//
// Ctrl-Break rather than Ctrl-C: it is the only console event a program can
// aim at one process group, and provision handles the two alike. Aiming
// Ctrl-C at the whole console would stop the test harness with it.
#[cfg(windows)]
#[test]
#[expect(unsafe_code, reason = "the console API has no safe equivalent in std")]
fn ctrl_break_stops_the_run_and_still_prints_its_summary() {
    use std::os::windows::process::CommandExt;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn AllocConsole() -> i32;
        fn GenerateConsoleCtrlEvent(event: u32, group: u32) -> i32;
    }
    const CTRL_BREAK_EVENT: u32 = 1;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

    let dir = tempfile::tempdir().unwrap();
    let started = dir.path().join("started.txt");
    let survived = dir.path().join("survived.txt");
    std::fs::write(
        dir.path().join("plan.yml"),
        format!(
            "description: a plan to stop\nsteps:\n  - name: waits to be stopped\n    \
             shell: |\n      Set-Content '{}' x\n      {}\n      Set-Content '{}' x\n    \
             changed_when: \"false\"\n",
            started.display(),
            sleep_for(5),
            survived.display(),
        ),
    )
    .unwrap();

    // The event goes to processes sharing the sender's console. Under a
    // terminal there is one already and this fails harmlessly; a runner
    // with none gets one here, for provision to inherit.
    // SAFETY: no arguments, and it touches no memory of ours.
    unsafe {
        AllocConsole();
    }
    let child = Command::new(env!("CARGO_BIN_EXE_provision"))
        .args(["apply", "plan.yml"])
        .env("NO_COLOR", "1")
        .current_dir(dir.path())
        .creation_flags(CREATE_NEW_PROCESS_GROUP)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    // Wait for the step to be running rather than guessing how long
    // PowerShell takes to start.
    let begun = std::time::Instant::now();
    while !started.exists() {
        assert!(
            begun.elapsed() < std::time::Duration::from_secs(60),
            "the step never started"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    // SAFETY: plain integers. The group is provision's alone, made by
    // `CREATE_NEW_PROCESS_GROUP`, so the event does not reach this process.
    let sent = unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, child.id()) };
    assert_ne!(sent, 0, "the console refused the event");

    let out = child.wait_with_output().unwrap();
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(130), "{text}");
    assert!(
        text.contains("exit 130 · interrupted"),
        "the step was not reported as interrupted:\n{text}"
    );
    assert!(
        text.contains("1 step · 1 failed"),
        "the summary is still printed:\n{text}"
    );
    // The event reached provision's group and not the step's, so the step
    // ends only if provision killed it.
    std::thread::sleep(std::time::Duration::from_secs(6));
    assert!(
        !survived.exists(),
        "the step outlived the run that was stopped"
    );
}

// ── `json`: a file with two owners (spec §6.11, D31) ──────────────────────

/// Write a JSON file into the test's own tempdir.
fn seed_json(dir: &Path, name: &str, text: &str) {
    std::fs::write(dir.join(name), text).expect("writing into the test's own tempdir");
}

fn read(dir: &Path, name: &str) -> String {
    std::fs::read_to_string(dir.join(name)).expect("the step should have left a file")
}

/// Spec §11's definition of idempotent, and §6.11's whole reason to exist:
/// the keys the application owns are still there after the apply, and the
/// second apply has nothing to do.
#[test]
fn a_json_step_declares_its_own_keys_and_leaves_the_rest_alone() {
    let dir = tempfile::tempdir().unwrap();
    seed_json(
        dir.path(),
        "settings.json",
        r#"{"model": "sonnet", "effortLevel": "high", "permissions": {"allow": ["Bash(tmux:*)"]}}"#,
    );
    let plan = "  - name: settings\n    json:\n      path: ./settings.json\n      content:\n        permissions:\n          deny: [\"Bash(rm -rf:*)\"]\n";

    let (code, out) = apply(dir.path(), plan, &[]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(verdict(&out, "settings"), "changed", "{out}");

    let after: serde_json::Value =
        serde_json::from_str(&read(dir.path(), "settings.json")).unwrap();
    // The application's keys, untouched — the four that #54 lost on every run.
    assert_eq!(after["model"], "sonnet");
    assert_eq!(after["effortLevel"], "high");
    // A sibling key under a declared parent survives: `permissions.allow` was
    // never declared, so it is not the plan's to replace.
    assert_eq!(after["permissions"]["allow"][0], "Bash(tmux:*)");
    // And what the plan does declare is there.
    assert_eq!(after["permissions"]["deny"][0], "Bash(rm -rf:*)");

    let (code, out) = apply(dir.path(), plan, &[]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(verdict(&out, "settings"), "ok", "{out}");
}

/// §6.11: everything that is not a mapping is a leaf, arrays included. A
/// declared list is the plan's whole list, which is what keeps it prunable.
#[test]
fn a_declared_array_is_replaced_whole_and_not_appended_to() {
    let dir = tempfile::tempdir().unwrap();
    seed_json(
        dir.path(),
        "a.json",
        r#"{"tools": ["old", "stale"], "keep": 1}"#,
    );
    let plan = "  - name: tools\n    json:\n      path: ./a.json\n      content:\n        tools: [\"new\"]\n";

    let (code, out) = apply(dir.path(), plan, &[]);
    assert_eq!(code, 0, "{out}");
    let after: serde_json::Value = serde_json::from_str(&read(dir.path(), "a.json")).unwrap();
    assert_eq!(after["tools"], serde_json::json!(["new"]));
    assert_eq!(after["keep"], 1);

    let (_, out) = apply(dir.path(), plan, &[]);
    assert_eq!(verdict(&out, "tools"), "ok", "{out}");
}

/// §6.11: a target that is missing is created holding exactly the declared
/// value, parents included — §6.3's rule, which every creating action shares.
#[test]
fn a_json_step_creates_a_missing_target_and_its_parents() {
    let dir = tempfile::tempdir().unwrap();
    let plan = "  - name: fresh\n    json:\n      path: ./nested/deep/new.json\n      content:\n        a: 1\n";

    let (code, out) = apply(dir.path(), plan, &[]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(verdict(&out, "fresh"), "changed", "{out}");
    let after: serde_json::Value =
        serde_json::from_str(&read(dir.path(), "nested/deep/new.json")).unwrap();
    assert_eq!(after, serde_json::json!({"a": 1}));

    let (_, out) = apply(dir.path(), plan, &[]);
    assert_eq!(verdict(&out, "fresh"), "ok", "{out}");
}

/// §6.11: the verdict is on parsed values, never on bytes. A target whose
/// formatting differs from what this action emits is left alone, or it would
/// be rewritten on every run forever — §2's idempotency, for the whole class.
#[test]
fn formatting_that_differs_is_not_a_change() {
    let dir = tempfile::tempdir().unwrap();
    let odd = "{\n    \"a\": 1,\n        \"b\": 2\n}";
    seed_json(dir.path(), "f.json", odd);
    let plan = "  - name: fmt\n    json:\n      path: ./f.json\n      content:\n        a: 1\n";

    let (code, out) = apply(dir.path(), plan, &[]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(verdict(&out, "fmt"), "ok", "{out}");
    assert_eq!(
        read(dir.path(), "f.json"),
        odd,
        "the bytes were not touched"
    );
}

/// §6.11 and §10: a target this action cannot read holds state provision did
/// not put there. It fails, and the file is still exactly as it was.
#[test]
fn a_target_that_is_not_json_fails_and_is_not_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let junk = "this is not json at all\n";
    seed_json(dir.path(), "bad.json", junk);
    let plan = "  - name: junk\n    json:\n      path: ./bad.json\n      content:\n        a: 1\n";

    let (code, out) = apply(dir.path(), plan, &[]);
    assert_ne!(code, 0, "{out}");
    assert!(out.contains("not valid JSON"), "{out}");
    assert_eq!(read(dir.path(), "bad.json"), junk, "the file must survive");
}

/// The same rule for a document that parses: there are no keys to merge into
/// an array, and replacing it would be the data loss this action prevents.
#[test]
fn a_target_whose_top_level_is_not_an_object_fails() {
    let dir = tempfile::tempdir().unwrap();
    let list = "[1, 2, 3]";
    seed_json(dir.path(), "arr.json", list);
    let plan = "  - name: arr\n    json:\n      path: ./arr.json\n      content:\n        a: 1\n";

    let (code, out) = apply(dir.path(), plan, &[]);
    assert_ne!(code, 0, "{out}");
    assert!(out.contains("not a JSON object"), "{out}");
    assert_eq!(read(dir.path(), "arr.json"), list, "the file must survive");
}

/// §6.11: `src` is the same declaration kept in a file beside the plan.
#[test]
fn the_declared_keys_can_come_from_a_src_file() {
    let dir = tempfile::tempdir().unwrap();
    seed_json(dir.path(), "owned.json", r#"{"a": {"b": 2}}"#);
    seed_json(dir.path(), "t.json", r#"{"a": {"c": 3}, "z": 9}"#);
    let plan = "  - name: src\n    json:\n      path: ./t.json\n      src: ./owned.json\n";

    let (code, out) = apply(dir.path(), plan, &[]);
    assert_eq!(code, 0, "{out}");
    let after: serde_json::Value = serde_json::from_str(&read(dir.path(), "t.json")).unwrap();
    assert_eq!(after, serde_json::json!({"a": {"b": 2, "c": 3}, "z": 9}));

    let (_, out) = apply(dir.path(), plan, &[]);
    assert_eq!(verdict(&out, "src"), "ok", "{out}");
}

/// §6.11: `plan` reports one line per changed leaf, keyed by its dotted path,
/// and never a unified text diff.
#[test]
fn plan_names_the_leaves_it_would_change() {
    let dir = tempfile::tempdir().unwrap();
    seed_json(dir.path(), "p.json", r#"{"model": "sonnet"}"#);
    std::fs::write(
        dir.path().join("plan.yml"),
        "description: a plan\nsteps:\n  - name: leaves\n    json:\n      path: ./p.json\n      content:\n        model: \"opus\"\n        new: true\n",
    )
    .unwrap();

    let (code, out) = run(dir.path(), &["plan", "plan.yml"]);
    // §8: `plan` with work pending exits 2.
    assert_eq!(code, 2, "a pending change is exit 2: {out}");
    assert!(out.contains(r#"model: "sonnet" → "opus""#), "{out}");
    assert!(out.contains("new: absent → true"), "{out}");
    assert!(
        !out.contains("@@"),
        "a unified diff has no business here: {out}"
    );
    // Read-only: `plan` leaves the file alone.
    assert_eq!(read(dir.path(), "p.json"), r#"{"model": "sonnet"}"#);
}

// ── the rendered run (§9.1, §9.2) ─────────────────────────────────────────
//
// Every snapshot of apply's terminal output lived in `tests/apply.rs`, which
// is `#![cfg(unix)]`, so the Windows job pinned the verdicts, the `--json`
// shape and the deadline — and not one rendered line (#60). These three are
// that render, one file each and not one per platform: the name column is a
// fixed width and not the console's, the glyphs are the same characters
// everywhere, and a path provision *prints* is written with `/` on every
// platform (D29). A separator in the output is the point, so the plan is
// nested a directory deep and the template is a tree.
//
// If one ever does differ, the answer is to fix the render and not to suffix
// the snapshot: a second file per snapshot is the cost #60 names as the
// reason this gap stayed open.

/// Durations are the only thing in the output that changes between runs —
/// `tests/apply.rs` filters them the same way.
macro_rules! snapshot {
    ($name:expr, $text:expr) => {
        insta::with_settings!(
            {filters => vec![
                (r"\d+ms", "[t]"),
                (r"\d+\.\d+s", "[t]"),
            ]},
            { insta::assert_snapshot!($name, $text) }
        )
    };
}

/// `apply`, with the plan written somewhere other than the tempdir's root.
///
/// The path provision prints is the one the operator named, so a plan one
/// directory down is what puts a separator in the header and the summary —
/// the line D29 is about.
fn apply_at(dir: &Path, rel: &str, body: &str, extra: &[&str]) -> (i32, String) {
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().expect("a relative plan path has a parent"))
        .expect("creating the plan's directory in the test's own tempdir");
    std::fs::write(&path, format!("description: a nested plan\nsteps:\n{body}"))
        .expect("writing into the test's own tempdir");
    let mut args = vec!["apply", rel];
    args.extend_from_slice(extra);
    run(dir, &args)
}

/// A rendered path never carries a `\`, on any platform (D29).
///
/// The snapshot already pins this on whichever platform it was taken on;
/// this says out loud what would be wrong on the other one, so a Windows
/// failure reads as "a separator leaked" and not as "a snapshot moved".
fn no_backslashes(out: &str) {
    assert!(
        !out.contains('\\'),
        "D29: a printed path must use `/` on every platform:\n{out}"
    );
}

// §9.1: the header with its step count, one line per step with its glyph,
// name column and verdict, and §9.2's summary. The nested plan path is the
// D29 case.
#[test]
fn snapshot_the_rendered_run() {
    let dir = tempfile::tempdir().unwrap();
    let (code, out) = apply_at(
        dir.path(),
        "plans/run.yml",
        "  - name: an ungated shell is its own exit code\n    shell: \"exit 0\"\n    changed_when: \"false\"\n\
         \x20 - name: changed_when decides\n    shell: \"exit 0\"\n    changed_when: \"true\"\n\
         \x20 - name: a gate that says the work is done\n    shell: \"exit 0\"\n    when: \"false\"\n\
         \x20 - name: failed_when forgives a non-zero exit\n    shell: \"exit 3\"\n    failed_when: \"false\"\n",
        &[],
    );
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("plans/run.yml"), "{out}");
    no_backslashes(&out);
    snapshot!("rendered_run", out);
}

// §9.1: the diff block under a `file` step that creates its target.
#[test]
fn snapshot_the_file_diff_block() {
    let dir = tempfile::tempdir().unwrap();
    let (code, out) = apply(
        dir.path(),
        "  - name: a file that is not there yet\n    file:\n      path: ./out/hello.txt\n      content: |\n        one\n        two\n",
        &[],
    );
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("@@"), "a unified diff must show:\n{out}");
    no_backslashes(&out);
    snapshot!("file_diff_block", out);
}

// §6.4 and §9.1: a `template` tree reports one diff per output, headed by the
// output's path relative to `src`. That label is built from a `Path`, so it
// is the render most likely to spell a separator the OS's way — which is the
// bug this asserts is not there.
#[test]
fn snapshot_the_template_tree_diff_block() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("tpl/sub")).unwrap();
    std::fs::write(dir.path().join("tpl/top.conf.j2"), "top = {{ 1 + 1 }}\n").unwrap();
    std::fs::write(dir.path().join("tpl/sub/nested.conf.j2"), "nested = yes\n").unwrap();

    let (code, out) = apply(
        dir.path(),
        "  - name: a tree of templates\n    template:\n      src: ./tpl\n      dest: ./rendered\n",
        &[],
    );
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("sub/nested.conf"), "{out}");
    no_backslashes(&out);
    snapshot!("template_tree_diff_block", out);
}
