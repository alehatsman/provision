#!/usr/bin/env python3
"""Read a `provision --json` stream and assert what spec §9.3 promises.

This is the half of D24 that makes the gate evidence rather than decoration.
Running the gate proves provision can run a job; reading the stream the way a
runner would is what proves the contract D19 wrote down — one object per step
as it finishes, a summary that says whether the run was stopped, and counts
that agree with the steps that produced them.

Deliberately stdlib-only and deliberately strict: a checker that shrugs at a
malformed line is a checker that would not have caught the thing it is here
for.
"""

import argparse
import json
import sys

# spec §9.3.
STEP_KEYS = {"event", "index", "name", "status", "duration_ms", "file", "line"}
APPLY_STATUSES = {"ok", "changed", "unknown", "skipped", "failed"}
PLAN_STATUSES = {"would_change", "would_run", "would_run_unprobed"}
STATUSES = APPLY_STATUSES | PLAN_STATUSES
# The summary counts apply's verdicts. A plan stream is read for shape only.
SUMMARY_COUNTS = ("changed", "ok", "skipped", "unknown", "failed")


def fail(msg):
    print(f"check-stream: {msg}", file=sys.stderr)
    sys.exit(1)


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("path", help="file holding the --json stream")
    ap.add_argument(
        "--expect-interrupted",
        action="store_true",
        help="the summary must report interrupted: true",
    )
    ap.add_argument(
        "--expect-deadline",
        action="store_true",
        help="the summary must report deadline_exceeded: true",
    )
    ap.add_argument(
        "--expect-steps",
        type=int,
        default=1,
        help="the stream must carry at least this many step events",
    )
    ap.add_argument(
        "--expect-unrun",
        metavar="NAME",
        help="no step event may carry this name (the walk must have stopped)",
    )
    args = ap.parse_args()

    try:
        with open(args.path, encoding="utf-8") as fh:
            raw = [ln for ln in (line.strip() for line in fh) if ln]
    except OSError as exc:
        fail(f"cannot read {args.path}: {exc}")

    if not raw:
        fail(f"{args.path} is empty — a run always emits a summary")

    events = []
    for n, line in enumerate(raw, 1):
        try:
            events.append(json.loads(line))
        except json.JSONDecodeError as exc:
            fail(f"line {n} is not JSON: {exc}\n  {line[:200]}")

    for n, ev in enumerate(events, 1):
        if not isinstance(ev, dict) or "event" not in ev:
            fail(f"line {n} is not an event object")

    steps = [e for e in events if e["event"] == "step"]
    summaries = [e for e in events if e["event"] == "summary"]
    other = {e["event"] for e in events} - {"step", "summary"}
    if other:
        fail(f"unknown event kinds: {sorted(other)}")

    # A runner reads the summary to know the job ended. Exactly one, and last.
    if len(summaries) != 1:
        fail(f"expected exactly one summary event, got {len(summaries)}")
    if events[-1]["event"] != "summary":
        fail("the summary is not the last event — a runner reads it to stop")
    summary = summaries[0]

    if len(steps) < args.expect_steps:
        fail(f"expected at least {args.expect_steps} step events, got {len(steps)}")

    last_index = None
    for ev in steps:
        missing = STEP_KEYS - ev.keys()
        if missing:
            fail(f"step {ev.get('name')!r} is missing {sorted(missing)}")
        if ev["status"] not in STATUSES:
            fail(f"step {ev['name']!r} has unknown status {ev['status']!r}")
        if not isinstance(ev["duration_ms"], int):
            fail(f"step {ev['name']!r} has a non-integer duration_ms")
        if not isinstance(ev["line"], int):
            fail(f"step {ev['name']!r} has a non-integer line")
        if last_index is not None and ev["index"] <= last_index:
            fail(f"step indexes are not increasing: {last_index} then {ev['index']}")
        last_index = ev["index"]
        # rc/stdout/stderr ride together or not at all.
        present = {k for k in ("rc", "stdout", "stderr") if k in ev}
        if present and present != {"rc", "stdout", "stderr"}:
            fail(f"step {ev['name']!r} carries {sorted(present)} but not all three")

    if args.expect_unrun:
        ran = [e["name"] for e in steps if e["name"] == args.expect_unrun]
        if ran:
            fail(f"{args.expect_unrun!r} reported a verdict; the walk did not stop")

    # Both always present, both booleans — a runner has to tell a job that
    # finished from one that was stopped, and an absent key is not that answer.
    for key in ("interrupted", "deadline_exceeded"):
        if key not in summary:
            fail(f"the summary is missing {key}")
        if not isinstance(summary[key], bool):
            fail(f"the summary's {key} is {summary[key]!r}, not a boolean")

    if args.expect_interrupted and not summary["interrupted"]:
        fail("expected interrupted: true in the summary")
    if args.expect_deadline and not summary["deadline_exceeded"]:
        fail("expected deadline_exceeded: true in the summary")

    # The counts have to agree with the steps that produced them, or the
    # summary is a second source of truth rather than a restatement.
    if not any(e["status"] in PLAN_STATUSES for e in steps):
        for key in SUMMARY_COUNTS:
            if key not in summary:
                fail(f"the summary is missing the {key} count")
            counted = sum(1 for e in steps if e["status"] == key)
            if summary[key] != counted:
                fail(
                    f"the summary says {key}={summary[key]} but {counted} step "
                    f"events carry that status"
                )

    print(
        f"check-stream: {len(steps)} step events, counts agree, "
        f"interrupted={summary['interrupted']} "
        f"deadline_exceeded={summary['deadline_exceeded']}"
    )


if __name__ == "__main__":
    main()
