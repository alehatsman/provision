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
| Process | `std::process` + `wait-timeout` | no tokio |
| Diff | `similar` | unified diffs with color |
| Terminal | `anstyle` + `anstream`, `indicatif` for the spinner | NO_COLOR, non-TTY fallback for free |
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

**Status: code complete, gate open.** 45 tests green. What shipped differs
from the plan below in three places, each recorded: `saphyr` replaced
`serde_yaml` (D13), `plan` binds registers to a placeholder (D14), and
`validate.rs` folded into `expand.rs` — validate and plan are the same walk
with different reporting, so a plan cannot succeed on a config validate
rejects. The remaining gate work is the `~/dotfiles` mechanical rewrite
(migration.md §7 step 1), which lands in the dotfiles repo, not this one.

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

- Idempotency tests for `shell` with `creates` and with `unless`.
- Snapshot tests: TTY, plain, json, failure block, retry rendering.
- Timeout test proves the child's children die.
- `~/dotfiles/components/ssh/index.yml` applies on a scratch `$HOME` twice:
  changed, then ok.

### Phase 2 — `file`, `template`, `pkg`, `service`

Deliverables

- `file`: file/dir/absent/link, content/src, mode, owner, atomic write,
  sudo install path, diff in plan.
- `template`: render + `file` semantics, diff in plan.
- `pkg`: apt, pacman, yay, brew (+cask), winget. Query-before-install, one
  install call per step, `latest`.
- `service`: systemd system/user, launchd.

Gate

- Idempotency tests for all four. `pkg` tests run in containers
  (`archlinux`, `ubuntu`) and on the host for brew; winget test is manual
  and documented as such.
- `provision plan` on x1 (already converged by mooncake) shows zero changes
  except the predicted list from migration.md §4.

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
