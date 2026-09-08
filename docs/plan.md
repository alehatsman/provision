# provision — build plan

Status: phases 0–4 code complete · 2026-09-08

## Where this stands

| Phase | Gate | Closed at |
|---|---|---|
| 0 — parse, validate, plan | five machine plans validate and render | `c5a310b` |
| 1 — execution core | `shell`/`cmd`/`assert` run twice, changed then ok | `e88c41f` |
| 2 — typed actions | `file`/`template`/`pkg`/`service`, container tests green | `0cb5e2f` |
| 3 — tags, polish, Windows | Windows all-targets clean, self-contained `.exe` | `1a5c5f6` |
| 4 — migration and cut-over | dotfiles applies with provision, main_pc reports no `unknown` | dotfiles `91415fc` |

Windows was exercised natively on main_pc's host on 2026-09-08, from WSL via
`powershell.exe`, with the cross-compiled `x86_64-pc-windows-gnu` binary run
from a Windows path: `facts` reports the right `os`/`home`/`username`,
`validate` and `plan` (both with and without probing) run against
`platforms\windows\bootstrap.yml`, and a throwaway plan under `%TEMP%`
applied twice — changed, then ok — covering the powershell interpreter,
`file`, `template`, `register`, a `when` reading it, `creates`, `unless` and
`failed_when`. Exit codes 0, 2 and 3 all correct. What is still untested
there: `pkg` against winget, `service`, and Ctrl-C.

Open, and all of it the owner's — none of it can close from this machine:

- Apply to each machine, main_pc WSL first and x1 last (migration.md §7
  steps 2–7). Every gate below that line is a real apply.
- Build `provision-ci:latest` on main_pc. dotfiles CI is red until it exists.
- ~~Rulings~~ — all six settled 2026-09-08. D1's windows-gnu amendment and
  D16 confirmed as written. The hung-`unless` failure confirmed.
  `--explain-var` stays spec'd and unbuilt, marked as such in §10.
  mooncake stays installed everywhere: a tool, not a dependency.
  `--keep-going` built — spec §8, `main.rs` and `expand.rs`.
- The tag, a remote, and any push. `main` exists at `f788571`; the repo
  has no remote and nothing has been pushed.

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

Semantics are settled in spec §6.3–§6.6 and gathered in D16, confirmed by
the owner 2026-09-08.

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
  Runtime state disappears at logout, so the worst case cleans itself. It
  works: systemd reads `$XDG_RUNTIME_DIR/systemd/user`, and start, stop and
  restart are all covered, applied twice.

  Setting `enabled` is **not** covered and is manual. `systemctl --user
  enable` writes its symlink into `~/.config/systemd/user` whatever the unit's
  own directory, and no test of ours may write there. `--runtime` would move
  the symlink but also change the meaning — enablement that does not survive a
  reboot is not what a provisioning tool means by enabled — so the action does
  not pass it. The `enabled` *probe* is read-only and is tested.
- launchd is manual. There is no mac in this loop.

Not part of the gate, and said plainly rather than counted green: `provision
plan` on x1 showing zero changes against a mooncake-converged machine. x1 is
not the development box and cannot be reached from it. It waits for the owner.

### Phase 3 — tags, polish, Windows

Deliverables

- **Done** (`df451a8`, `e88c41f`) `--tags`, `--skip-tags`, `always`, applied
  post-expansion. `expand.rs:59`.
- **Done** (`df451a8`) `--hide-skipped`, `--no-diff`, `--stream`,
  `--verbose`, `--color`. All in `main.rs`.
- **Not implemented, deliberately** `--explain-var`. Nothing in the fleet
  asks for it: `validate`'s undefined-variable diagnostics already carry
  `file:line:col` and the name. Ruled 2026-09-08 — it stays written down in
  §10, marked there as spec'd but unbuilt, rather than being built or cut.
- **Done** Windows build: PowerShell is the default interpreter
  (`actions/mod.rs:48`), `~` expands, `sudo` is rejected at `model.rs:284`,
  and the process-group and `taskkill` paths exist (`process.rs:247,274`).
- **Done** (`df451a8`) `validate --strict`.

Gate

- **Done** (`tests/cli.rs:228,250`) tag tests for the two mooncake bugs
  (#191, #196): a positive tag filter excludes untagged steps; plan with a
  tag does not touch excluded steps.
- **Done** the binary cross-compiles and is self-contained.
  `cargo check --target x86_64-pc-windows-gnu --all-targets` is clean, and
  the 2.6 MB `.exe` imports system DLLs only — no `libgcc`, no
  `libwinpthread`. See the D1 amendment.
- **Partly.** Windows bootstrap plan validates and plans *on Linux*:
  `validate --vars-file machines/main_pc/vars.yml platforms/windows/bootstrap.yml`
  exits 0 for both Windows machines, and `plan --plan-no-probe` reports 18
  steps, 16 unprobed, 2 skipped, exit 0. Without a vars file it is 10
  undefined variables and exit 3, which is the file header's own
  instruction being enforced. Running `validate` and `plan` **on a Windows
  box** is not something this machine can do, and is not counted green. It
  waits for the owner, like x1.

### Phase 4 — migration and cut-over

Deliverables

- **Rewritten 2026-09-08 by review, confirmed by the owner the same day:
  mooncake is a tool, not a dependency, and stays installed.** This used to
  say "mooncake removed from every machine's plan and from
  `shared/bootstrap.yml`" and "`mooncake` binary uninstalled from all five
  machines". Both are dead as written, and migration.md §5 says why:
  mooncake stays a tool these plans provision around. `components/mooncake`
  builds a CI image, `components/fleet-peer` runs `mooncake agentd
  bootstrap`, the machine vars feed that agentd — and moongit's runner execs
  every CI step as `mooncake step '<yaml>'`, so the CI image has to carry
  mooncake no matter which tool applies the dotfiles. What phase 4 actually
  delivers is that **provision is the applier**. Whether the owner uninstalls
  anything afterwards is his call and not a gate.
- **Done** (dotfiles `6ff625f`…`91415fc`) `~/dotfiles` applies with
  provision. `tasks.yml` and `mooncake.yml` are gone from the branch, the
  `justfile` carries the apply recipes with `--json` run logs (D9), and
  every machine plan validates.
- **Done** (dotfiles `91415fc`) dotfiles CI runs `provision validate` +
  `provision plan --plan-no-probe` on all five machine plans, in an image
  built by `components/provision`.
- **Done** (dotfiles `d92ed9c`) every step on main_pc answers what it did:
  `plan` reports no `unknown`, and `validate --strict main_pc.yml` is clean.

Gate

- `provision apply` converges each machine from its mooncake-converged state
  with zero unexpected changes, then `plan` shows nothing to do. **Owner's**,
  and the substance of migration.md §7 steps 2–6: every one of these is a
  real apply on a machine, x1 first.
- One fresh machine (a VM or the next reinstall) bootstraps end to end.
  **Owner's**, for the same reason.
- Building `provision-ci:latest` on main_pc. **Owner's** — CI stays red
  until it exists, which is why that switch landed last and alone.
- `just` was a new fleet dependency and the owner's call. Ruled 2026-09-08:
  it goes in all three platform package lists. Closed.

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
