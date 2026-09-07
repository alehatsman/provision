# provision — build plan

Status: draft v0.1 · 2026-09-07

Rust, one crate, one binary. Target: **6–9k lines of Rust** including
tests, all seven actions, three platforms. If it passes 12k, something
from the non-goals crept in. Stop and cut.

## Ground rules

- Spec first. `docs/spec.md` changes before the code does.
- No phase starts until the previous phase's gate is green.
- No feature enters without a line in the spec and a step in `~/dotfiles`
  that needs it. "Might be useful" is a non-goal by definition.
- Every action: an idempotency test (run twice, `changed` then `ok`).
- Every output form: a snapshot test (insta), TTY and non-TTY.
- Boring crates, pinned. No async runtime.

## Crates

| Need | Crate | Why |
|---|---|---|
| CLI | `clap` (derive) | standard |
| YAML | `saphyr` (`MarkedYaml`) | spanned nodes, map keys included — the only way the "every error carries file:line" gate holds. Replaces the planned `serde_yaml`; see D13 |
| Templates + expressions | `minijinja` | Jinja2-compatible, evaluates `when` expressions too, one engine for both |
| Errors | one `Diag` type (`file:line:col` + message + note) | no `anyhow`/`thiserror`: every user-facing error is positional, and a collector reports all of them in one pass rather than the first |
| Process | `std::process` + `libc` (unix) | no tokio. `wait-timeout` was dropped: a watchdog thread gives one kill path for both `timeout` and Ctrl-C, and the kill itself needs `libc::killpg` either way. `Command::process_group` is std |
| Diff | `similar` | unified diffs with color |
| Terminal | `anstyle` + `anstream` | NO_COLOR, non-TTY fallback for free. `indicatif` was dropped: the spinner is one live line, and `\r` + erase is 40 lines against several transitive deps |
| Paths | `home`/`dirs` for `~`, `which` for manager detection | |
| Temp files | `tempfile` | atomic writes |
| Tests | `assert_cmd`, `insta`, `tempfile` | |

Not used: any plugin system, any async, any config-format abstraction,
any procedural-macro registry of actions. Actions are a Rust `enum`; the
dispatcher is a `match`.

## Architecture (small on purpose)

```
src/
  main.rs          clap, command dispatch, exit codes
  error.rs         Diag (file:line:col) and the collector
  yaml.rs          spanned nodes over saphyr, typed accessors, unknown-key check
  config/
    model.rs       Step, Mods, key vocabulary, modifier checks
    load.rs        file resolution, doc cache, component + prop schemas
  expand.rs        the sequential walk: scope, when, import/use, tags
  scope.rs         variable scopes, precedence, props, facts, env
  template.rs      minijinja env, filters, strict undefined, expr eval
  facts.rs
  exec/
    runner.rs      sequential loop, when/unless/creates, retry, timeout, register
    sudo.rs
    process.rs     spawn, capture, kill process group
  actions/
    shell.rs cmd.rs file.rs template.rs pkg.rs service.rs assert.rs
    mod.rs         trait Action { fn plan(&self, ctx) -> Probe; fn apply(&self, ctx) -> Outcome }
  output/
    tty.rs plain.rs json.rs
    event.rs       one event type both apply and plan emit
tests/
  fixtures/        small plans
  idempotency/     one per action
  snapshots/       output
```

A `Probe` is what `plan` prints; an `Outcome` is what `apply` prints. Both
carry `status` ∈ {ok, changed, skipped, unknown, failed} and an optional
diff or message. The output layer knows nothing about actions.

## Phases

### Phase 0 — parse, validate, render, plan-without-probe

**Status: done. Phase 0 is closed.** 46 tests green. What shipped differs
from the plan below in three places, each recorded: `saphyr` replaced
`serde_yaml` (D13), `plan` binds registers to a placeholder (D14), and
`validate.rs` folded into `expand.rs` — validate and plan are the same walk
with different reporting, so a plan cannot succeed on a config validate
rejects.

The `~/dotfiles` rewrite (migration.md §7 step 1) is done on that repo's
`provision` branch: all 58 YAML files, all five machine plans plus the
standalone Windows bootstrap `validate` clean and render every template
under `plan --plan-no-probe`. It found one hole here — action bodies were
not key-checked — now closed by `model::action_body_keys`. See
migration.md §8 and §9.

Phase 1 is next: the execution core.

Deliverables

- `config`: parse a plan list, one action key per step, unknown keys error,
  file:line in every error.
- `scope` + `template`: facts, `vars`, `vars_file`, `--var`, `--vars-file`,
  strict undefined, filters `expanduser` `basename` `dirname` `quote`.
- `when` evaluation. `import` expansion with cycle detection. `use` with
  prop schemas.
- `validate` and `plan --plan-no-probe` commands, plain output only.
- `facts` command.

Gate

- `provision validate` passes on all five `~/dotfiles` machine plans after
  the migration rewrite of structural keys (see migration.md §2). Every
  template in the 27 `.j2` files renders with `--plan-no-probe`.
- 100% of validation errors carry file:line. Snapshot-tested.

### Phase 1 — execution core, `shell`, `cmd`, `assert`

**Status: done. Phase 1 is closed.** 69 tests green. Four things differ from
the plan below, each recorded:

- `output/tty.rs` and `output/plain.rs` are one `output/text.rs` with two
  switches. §9.1 and §9.2 describe the same lines twice — once with a spinner
  and color, once without — and two renderers for that would drift apart.
- `actions/{shell,cmd,assert}.rs` are one `actions/mod.rs`. In phase 1 the
  three genuinely are one thing: build an argv, judge it by its exit code.
  Phase 2's actions each need real state inspection and get their own files;
  `Action::parse` is where their bodies go, and it already lives there.
- `trait Action { plan, apply }` became `trait Judge` pointing the other way.
  The runner asks the expander to evaluate `failed_when` *inside* the retry
  loop, which is what makes `retry` on an `assert` a readiness gate rather
  than a rerun of something already deemed fine.
- Expansion and execution interleave rather than running as two passes,
  because a `when` that reads a `register` needs the registering step to have
  actually run (D14). `apply` still walks twice: once exactly as `validate`
  does, which is what lets the sudo preflight happen before the first step.

Deliverables

- `runner`: sequential execution, `unless`, `creates`, `timeout` with
  process-group kill, `retry`, `env`, `cwd`, `register`, `changed_when`,
  `failed_when`, Ctrl-C handling.
- `sudo`: `-n` preflight, `--ask-sudo-pass`.
- `shell` (bash/sh/zsh/powershell/pwsh), `cmd`, `assert`.
- TTY output with spinner, plain output, `--json`. Failure block with
  stderr tail.
- `apply` command.

Gate

- Idempotency tests for `shell` with `creates` and with `unless`, and for
  `cmd` — applied twice, changed then skipped.
- Snapshot tests: plain, json, failure block, retry rendering. The TTY
  snapshot is not part of the gate: a captured run has no terminal, so what
  it would pin is the color mapping, not the spinner. That test exists
  anyway, because the color mapping is worth pinning.
- Timeout test proves the child's children die.
- `env` reaches the step and, under `sudo`, nothing else does.
- `cwd` defaults to the plan file's own directory and `cwd:` overrides it.
- A scratch-`$HOME` fixture applies twice — changed, then ok — carrying the
  three shapes `components/ssh/index.yml` uses: an `unless`-gated shell, a
  `creates`-gated shell, and an `assert` with `retry`.

Two things are not covered by a test, and are said plainly rather than counted
as green. `--ask-sudo-pass` needs a terminal to type into. And `plan`'s soft
sudo preflight — the path where root is *not* reachable and a root gate leaves
its step unprobed — could not be exercised on the development machine, which
is configured `NOPASSWD`, so `sudo -n` succeeds even after `sudo -k`. Both
want the privileged container phase 2's `pkg` tests bring. `--preserve-env` is
tested where a warm `sudo -n` exists and skips itself where it does not.

The ssh component itself is a **Phase 2** gate, not this one: its first step
is `file: {path: ~/.ssh, state: dir, mode: '0700'}`, and `file` does not
exist until Phase 2. Gating Phase 1 on it would mean shipping `file` half
built across two phases. Decided 2026-09-08.

### Phase 2 — `file`, `template`, `pkg`, `service`

Deliverables

- `file`: file/dir/absent/link, content/src, mode, owner, atomic write,
  sudo install path, diff in plan.
- `template`: render + `file` semantics, diff in plan.
- `pkg`: apt, pacman, yay, brew (+cask), winget. Query-before-install, one
  install call per step, `latest`.
- `service`: systemd system/user, launchd.

Semantics are settled in spec §6.3–§6.6 and gathered in D16, which is
provisional pending the owner's review.

Gate

- Idempotency tests for all four, run twice on a scratch directory: changed,
  then ok or skipped. For `file` that means each state — file, dir, link,
  absent — plus a mode change on an existing directory and the sudo `install`
  path. The sudo test skips itself where `sudo -n true` fails.
- `examples/components/ssh/index.yml` applies on a scratch `$HOME` twice:
  changed, then ok. Moved here from Phase 1, because its first step is a
  `file` action. The `~/dotfiles` copy is the owner's repo and his to run.
- `pkg` runs in containers, never on the host: `docker run --rm` against
  `ubuntu:24.04` and `archlinux:latest` with the debug binary and a fixture
  directory mounted, installing something small. These tests are `#[ignore]`
  unless **`PROVISION_CONTAINER_TESTS=1`** is set, so a plain `cargo test`
  never needs a daemon. brew and winget are manual and documented as such.
- `service` uses a throwaway unit in the **runtime** unit directory
  (`$XDG_RUNTIME_DIR/systemd/user`), never `~/.config/systemd/user` — that
  directory holds the owner's live units, and a test process killed between
  writing and cleaning up would leave a stray unit in his config forever.
  Runtime state disappears at logout, so the worst case cleans itself. One
  attempt: if systemd does not pick the unit up there, the test skips with a
  message and `service` joins launchd and winget as manual.
- launchd is manual. There is no mac in this loop.

Not part of the gate, and said plainly rather than counted green: `provision
plan` on x1 showing zero changes against a mooncake-converged machine. x1 is
not the development box and cannot be reached from it. It waits for the owner.

### Phase 3 — tags, polish, Windows

Deliverables

- `--tags`, `--skip-tags`, `always`, applied post-expansion.
- `--hide-skipped`, `--no-diff`, `--stream`, `--verbose`, `--explain-var`.
- Windows build: `shell` with PowerShell default, `~` expansion, `sudo`
  rejected with a clear message.
- `validate --strict`.

Gate

- Tag tests for the two mooncake bugs (#191, #196): a positive tag filter
  excludes untagged steps; plan with a tag does not touch excluded steps.
- Windows bootstrap plan validates and plans on a Windows box.

### Phase 4 — migration and cut-over

Deliverables

- `~/dotfiles` fully on provision. mooncake removed from every machine's
  plan and from `shared/bootstrap.yml`.
- dotfiles CI runs `provision validate` + `provision plan --plan-no-probe`
  on all machine plans.
- `mooncake` binary uninstalled from all five machines.

Gate

- `provision apply` converges each machine from its mooncake-converged state
  with zero unexpected changes, then `plan` shows nothing to do.
- One fresh machine (a VM or the next reinstall) bootstraps end to end.

## Order and dependencies

```
P0 → P1 → P2 → P3 → P4
```

Strictly linear. P2 could start before P1's output polish is done, but
not before the runner exists. Do not parallelize with agents; the codebase
is small enough that merge cost exceeds the gain.

## What to measure

- Lines of Rust per phase, against the budget.
- `provision apply` wall time on x1 versus `mooncake apply`. Should be equal
  or better; the shell steps dominate both.
- Number of `unknown` verdicts on a converged machine. Target: zero after
  migration. Each one is a shell step missing its gate.

## Deferred, with the condition that reopens each

| Item | Reopens when |
|---|---|
| `line_in_file` / `text.replace` action | more than 3 shell steps in dotfiles reimplement sed-with-a-guard |
| `git` action (clone with ref) | more than 3 shell clones need pinned refs |
| `pkg` repo/tap/PPA management | a shell recipe from migration.md fails idempotency in practice |
| Loops | any step repeats itself more than 3 times by copy-paste |
| Windows typed actions | the PowerShell bootstrap exceeds 500 lines or breaks idempotency |
| Run log | a real question "what did the last apply do" goes unanswered twice |
| Remote apply (`ssh host provision apply`) | never as a feature; a shell alias suffices |
