#!/usr/bin/env python3
"""Prove `check-stream.py` still refuses what it is there to refuse.

The gate only ever points the checker at streams that are expected to pass
(`gate.yml`), so a checker that degraded to a no-op -- an early `return`, a
`fail()` that stopped exiting, a rule deleted with the code it guarded --
would leave the gate green and the contract unguarded. That is the one
failure a green gate cannot report, so it gets its own test: a known-bad
stream per rule, and the checker must exit 1 on every one of them.

Stdlib-only and run before anything is built, for the same reason the
checker is: nothing here should depend on the thing under test being
healthy.
"""

import json
import pathlib
import subprocess
import sys
import tempfile

HERE = pathlib.Path(__file__).resolve().parent
CHECKER = HERE / "check-stream.py"

MANIFEST = {"event": "manifest", "plan": "p.yml", "steps": 2, "files": 1, "sudo": False}


def step(index, name, status, **extra):
    ev = {
        "event": "step",
        "index": index,
        "name": name,
        "status": status,
        "duration_ms": 1,
        "file": "p.yml",
        "line": index,
    }
    ev.update(extra)
    return ev


def summary(**extra):
    s = {
        "event": "summary",
        "plan": "p.yml",
        "total": 2,
        "ok": 1,
        "changed": 1,
        "skipped": 0,
        "unknown": 0,
        "failed": 0,
        "would_change": 0,
        "would_run": 0,
        "would_run_unprobed": 0,
        "interrupted": False,
        "deadline_exceeded": False,
        "duration_ms": 2,
    }
    s.update(extra)
    return s


def good():
    """A stream that passes. Every bad case below is this, bent in one place."""
    return [
        MANIFEST,
        step(1, "first", "ok", rc=0, stdout="", stderr=""),
        step(2, "second", "changed", rc=0, stdout="", stderr=""),
        summary(),
    ]


def without(ev, key):
    out = dict(ev)
    out.pop(key)
    return out


def bend(fn):
    """Apply `fn` to a fresh good stream and return the result."""
    events = good()
    return fn(events) or events


# (name, stream or raw text, extra argv). Each must be refused.
BAD = [
    ("an empty stream", [], []),
    ("a line that is not JSON", "{not json}\n", []),
    ("a line that is not an event object", [MANIFEST, [1, 2, 3], summary()], []),
    ("an object with no event key", [MANIFEST, {"name": "x"}, summary()], []),
    (
        "an unknown event kind",
        bend(lambda e: e.insert(1, {"event": "progress"})),
        [],
    ),
    ("no summary at all", bend(lambda e: e.pop()), []),
    (
        "a summary that is not last",
        bend(lambda e: e.insert(1, e.pop())),
        [],
    ),
    ("two summaries", bend(lambda e: e.append(summary())), []),
    (
        "a manifest that is not first",
        bend(lambda e: e.insert(2, e.pop(0))),
        [],
    ),
    (
        "a manifest missing a key",
        bend(lambda e: e.__setitem__(0, without(MANIFEST, "sudo"))),
        [],
    ),
    (
        "a manifest claiming more steps than the summary counted",
        bend(lambda e: e.__setitem__(0, dict(MANIFEST, steps=9))),
        [],
    ),
    (
        "a step missing a required key",
        bend(lambda e: e.__setitem__(1, without(e[1], "duration_ms"))),
        [],
    ),
    (
        "a step with an unknown status",
        bend(lambda e: e[1].__setitem__("status", "maybe")),
        [],
    ),
    (
        "a non-integer duration_ms",
        bend(lambda e: e[1].__setitem__("duration_ms", "1ms")),
        [],
    ),
    (
        "step indexes that do not increase",
        bend(lambda e: e[2].__setitem__("index", 1)),
        [],
    ),
    (
        "stdout without rc",
        bend(lambda e: e.__setitem__(1, without(e[1], "rc"))),
        [],
    ),
    (
        "a passing step carrying rc with no stdout",
        bend(lambda e: e.__setitem__(1, without(without(e[1], "stdout"), "stderr"))),
        [],
    ),
    (
        "a failed step missing its message",
        bend(
            lambda e: e.__setitem__(
                2, step(2, "second", "failed", rc=None, stderr="boom")
            )
            or e.__setitem__(3, summary(changed=0, failed=1))
        ),
        [],
    ),
    (
        "a failed step that ran no command reporting a real rc",
        bend(
            lambda e: e.__setitem__(
                2,
                step(2, "second", "failed", rc=3, stderr="boom", message="boom"),
            )
            or e.__setitem__(3, summary(changed=0, failed=1))
        ),
        [],
    ),
    (
        "counts that disagree with the steps",
        bend(lambda e: e.__setitem__(3, summary(ok=2, changed=0))),
        [],
    ),
    (
        "a summary missing interrupted",
        bend(lambda e: e.__setitem__(3, without(summary(), "interrupted"))),
        [],
    ),
    (
        "a non-boolean deadline_exceeded",
        bend(lambda e: e.__setitem__(3, summary(deadline_exceeded="no"))),
        [],
    ),
    ("fewer steps than asked for", good(), ["--expect-steps", "5"]),
    ("a step that should not have run", good(), ["--expect-unrun", "second"]),
    ("an interrupt that is not reported", good(), ["--expect-interrupted"]),
    ("a deadline that is not reported", good(), ["--expect-deadline"]),
]


def write(tmp, name, stream):
    path = tmp / (name.replace(" ", "_") + ".jsonl")
    if isinstance(stream, str):
        path.write_text(stream, encoding="utf-8")
    else:
        path.write_text(
            "".join(json.dumps(e) + "\n" for e in stream), encoding="utf-8"
        )
    return path


def run(path, extra):
    return subprocess.run(
        [sys.executable, str(CHECKER), str(path), *extra],
        capture_output=True,
        text=True,
        check=False,
    )


def main():
    failures = []
    with tempfile.TemporaryDirectory() as td:
        tmp = pathlib.Path(td)

        # The control. If this does not pass, every refusal below proves
        # nothing -- a checker that refuses everything is as broken as one
        # that refuses nothing.
        ok = run(write(tmp, "a good stream", good()), [])
        if ok.returncode != 0:
            failures.append(f"a good stream was refused: {ok.stderr.strip()}")

        for name, stream, extra in BAD:
            got = run(write(tmp, name, stream), extra)
            if got.returncode == 0:
                failures.append(f"accepted {name}")
            elif got.returncode != 1:
                failures.append(
                    f"{name}: expected exit 1, got {got.returncode}\n"
                    f"  {got.stderr.strip()[:300]}"
                )

    if failures:
        for f in failures:
            print(f"check-stream-selftest: {f}", file=sys.stderr)
        sys.exit(1)
    print(f"check-stream-selftest: 1 good stream accepted, {len(BAD)} refused")


if __name__ == "__main__":
    main()
