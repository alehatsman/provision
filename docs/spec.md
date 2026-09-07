# provision — specification

Status: v0.2 · 2026-09-07 · owner: aleh

This is the contract. Code that disagrees with it is wrong, or this file is.
Fix one.

## 1. Goal

Converge one machine to a declared state from one YAML plan, from a single
static binary, on Linux, macOS, and Windows, with output a person can read
and a dry-run a person can trust.

## 2. Scope

**In**

- Parse and validate a plan. Render templates. Evaluate conditions.
- Execute steps sequentially on the local machine. Stop at the first failure.
- Seven actions: `shell`, `cmd`, `file`, `template`, `pkg`, `service`, `assert`.
- Four structural keywords: `vars`, `vars_file`, `import`, `use`.
- Step modifiers: `name`, `when`, `unless`, `creates`, `sudo`, `timeout`,
  `retry`, `env`, `cwd`, `tags`, `register`, `changed_when`, `failed_when`.
- Facts about the local machine.
- Three commands: `validate`, `plan`, `apply`.
- TTY and non-TTY output, `--json` event stream.

**Out** (see README non-goals and decisions.md)

- Remote execution, fleet, daemon, agent, MCP, SDK.
- Transactions, rollback, handlers/notify.
- Loops (`loop`, `with_items`).
- Secrets providers. Run log. State directory. Plugins. Modules registry.
- Typed Windows actions (registry, firewall, scheduled tasks). Windows is
  supported through `shell` with the PowerShell interpreter.
- Task runner.

## 3. Plan file

A plan is a YAML **list of steps**. A step is a mapping with exactly one
action key or one structural key, plus optional modifiers.

```yaml
- name: Install base packages       # modifier
  pkg:                              # action key
    names: [git, curl]
  sudo: true                        # modifier
  tags: [core]                      # modifier
```

Rules:

- Exactly one of the action or structural keys per step. Zero or two is a
  validation error naming the step and the file:line.
- Unknown top-level keys in a step are errors. No silent typos.
- Files are resolved relative to the file that names them, never the cwd.
- `~` expands in every path field the tool owns (`dest`, `path`, `src`,
  `creates`, `cwd`, `vars_file`, `import`, `use`). It does **not** expand
  inside shell scripts; the shell does that.

### 3.1 Structural keywords

| Key | Form | Meaning |
|---|---|---|
| `vars` | mapping | Set variables in the current scope. Later wins. |
| `vars_file` | path or list of paths | Load a YAML mapping into the current scope. Path may contain templates (`./vars/{{ os }}.yml`). Missing file is an error unless `optional: true` is set on the step. |
| `import` | path | Inline the steps of another plan file into this scope. `when` and `tags` on the import apply to every imported step (tags are unioned). |
| `use` | path | Run a **component** in a child scope. See §3.2. |

Scope: a plan file has one scope. `import` shares it. `use` creates a child
that sees the parent's variables read-only plus its own `props`, and whose
`vars` do not leak back.

### 3.2 Components

A component file is a mapping, not a list:

```yaml
props:
  variant:
    type: string          # string | bool | int | list
    default: dark
    description: palette variant
  notes_dir:
    type: string
    required: true

steps:
  - name: Render palette
    template: { src: ./{{ props.variant }}.j2, dest: ~/.config/palette }
```

Call site:

```yaml
- name: Palette
  use: ../components/palette/index.yml
  props: { variant: "{{ palette_variant }}" }
  tags: [core, palette]
```

- Unknown prop at the call site: error. Missing required prop: error.
  Wrong type after template rendering: error. All three at `validate`
  time, not at apply time.
- Inside the component, props are reachable as `props.<name>`.

### 3.3 Variables and precedence

Lowest to highest:

1. Facts (§5).
2. `vars` / `vars_file` in file order, later wins.
3. Command line `--var key=value` and `--vars-file path` (repeatable, later wins).
4. Inside a component, `props.*` (separate namespace, no collision).

`env.*` exposes the process environment read-only.

### 3.4 Templating

Jinja2 via minijinja. Applies to every string value in a step except the
body of `shell` **when** `raw: true` is set on that step. Templates in
`.j2` files rendered by `template` use the same engine and scope.

- Undefined variable: error at plan time, with file:line and the variable name.
- Filters available: minijinja built-ins plus `expanduser`, `basename`,
  `dirname`, `quote` (shell-quote), `to_yaml`, `to_json`.
- Tests available: minijinja built-ins plus `exists` (path).
- `{% raw %}` blocks are honored; needed for shell scripts with `{{`. This is
  Jinja2's tag. `{% verbatim %}` is Twig's and is **not** supported.

**A field that is one expression keeps its type.** When a string field is
exactly one `{{ … }}` and nothing else, it is evaluated rather than rendered,
and the result keeps the type the expression produced:

```yaml
vars:
  apps: [git, curl]

pkg:
  names: "{{ apps }}"        # the list, not its textual rendering
file:
  path: "{{ home }}/.zshrc"  # a string: there is text around the expression
```

Without this, `names: "{{ apps }}"` would be the string `['git', 'curl']`, and
§3.2's "wrong type after template rendering is an error" could never pass. The
rule is exactly "one expression, no surrounding text"; two expressions, or an
expression with text beside it, render to a string.

Booleans render Python-style, as Jinja2 does: `{{ is_wsl }}` is `True`. Use
`{{ is_wsl | to_json }}` where a config file needs `true`.

### 3.5 Conditions

`when` and `changed_when` / `failed_when` are minijinja expressions
evaluated to a boolean. Non-boolean results are an error, not truthy.

```yaml
when: os == "linux" and not is_wsl
when: pacman_available
failed_when: result.rc not in [0, 2]
changed_when: result.rc == 0
```

## 4. Step modifiers

| Modifier | Type | Default | Meaning |
|---|---|---|---|
| `name` | string | action key + first arg | Shown in output. Templated. |
| `when` | expr | true | Skip the step when false. Evaluated before `unless`/`creates`. |
| `unless` | string | — | A shell command. Exit 0 means "already done", step skipped. Non-zero means run. Any other failure to spawn is an error. |
| `creates` | path | — | Skip when the path exists. |
| `sudo` | bool | false | Run the action as root (§7). Windows: error; use `shell` from an elevated prompt. |
| `timeout` | duration | 10m | Kill the step and fail. `30s`, `5m`, `1h`. |
| `retry` | `{attempts, delay}` | none | Re-run on failure. `delay` is a duration. Output shows attempt N/M. |
| `env` | mapping | {} | Extra environment for `shell`, `cmd`, `unless`. Templated. |
| `cwd` | path | plan file dir | Working directory for `shell`, `cmd`, `unless`. |
| `tags` | list | [] | For `--tags` selection (§8). |
| `register` | identifier | — | Store the step result as a variable: `{rc, stdout, stderr, changed, skipped}`. |
| `changed_when` | expr | action-defined | Override the changed verdict. `result` is in scope. |
| `failed_when` | expr | `result.rc != 0` | Override the failure verdict. `result` is in scope. |
| `raw` | bool | false | Do not template the action body. |

Order of evaluation: `when` → `unless` → `creates` → action → `failed_when`
→ `changed_when` → `register`.

## 5. Facts

Computed once at startup, lazily where expensive, read-only.

| Fact | Values |
|---|---|
| `os` | `linux`, `darwin`, `windows` |
| `arch` | `x86_64`, `aarch64` |
| `hostname` | short hostname as the kernel reports it |
| `username`, `home` | current user |
| `distro`, `distro_version` | from `/etc/os-release` (`arch`, `ubuntu`, …), empty elsewhere |
| `is_wsl` | `/proc/version` contains `microsoft` |
| `apt_available`, `pacman_available`, `brew_available`, `winget_available`, `yay_available` | binary on PATH |
| `systemd_available`, `launchd_available` | init detection |

No user-declared facts. Anything else is a variable.

## 6. Actions

Every action defines: fields, what "changed" means, what `plan` reports,
and its idempotency guarantee. `changed` is a verdict, not a guess: an
action that cannot tell reports `unknown`, shown distinctly in output.

### 6.1 `shell`

Run a script through an interpreter.

```yaml
shell: |                       # short form: script string
  set -euo pipefail
  hostnamectl set-hostname x1

shell:                         # long form
  script: ...
  interpreter: bash            # bash (default on unix) | sh | zsh | powershell (default on windows) | pwsh
  login: false
```

- Idempotency: **declared**, via `unless`, `creates`, or `changed_when`. A
  bare `shell` step with none of these is reported `changed: unknown`.
  `validate --strict` fails on it.
- Plan: prints `would run` plus the first line of the script. If `unless` or
  `creates` is set, plan evaluates it and reports `skip` or `would run`.
  `--plan-no-probe` skips `unless` evaluation (for CI without the target).
- Streaming: stdout/stderr captured; shown in full on failure, on
  `--verbose`, or streamed live with `--stream`.
- `set -euo pipefail` is **not** injected. Explicit > magic.

### 6.2 `cmd`

Run an argv without a shell.

```yaml
cmd: [mv, "{{ home }}/Projects", "{{ home }}/projects"]
```

Same semantics as `shell` for idempotency, plan, output. Use when arguments
contain spaces or come from variables.

### 6.3 `file`

Ensure a filesystem entry.

```yaml
file:
  path: ~/.config/zsh
  state: dir                   # file | dir | absent | link
  content: "..."               # state=file only; mutually exclusive with src
  src: ./files/gitconfig       # state=file: copy; state=link: link target
  mode: "0644"
  owner: root                  # requires sudo
  group: wheel                 # requires sudo
```

- Changed when content, mode, owner, group, or type differs. Compared by
  bytes, not mtime.
- Plan: unified diff for content changes (`--no-diff` to suppress; secrets
  are the operator's problem), `mode 0644 → 0600` and `group staff → wheel`
  for metadata.
- `owner` and `group` are independent; either may be set alone. Both require
  `sudo: true` and are a validation error without it.
- Writes atomically: temp file in the same directory, rename. With `sudo`,
  the temp file is written as the user and installed with `install(1)`
  semantics so owner, group, and mode land correctly.
- `absent` on a non-empty dir requires `force: true`.

### 6.4 `template`

Render a `.j2` file and ensure `dest` matches, with `file` semantics.

```yaml
template:
  src: ./templates/.zshrc.j2
  dest: ~/.zshrc
  mode: "0644"
```

- Rendered in the step's scope. Undefined variable is an error at plan time.
- Plan: diff of rendered output against current file.
- Takes the same `mode`, `owner`, and `group` fields as `file` (§6.3), with
  the same meaning and the same sudo requirement.

**Directory mode.** When `src` names a directory, every file under it is
rendered into `dest`, keeping relative paths. Directories are created as
needed with mode `0755` (or `mode` masked to its directory form).

```yaml
template:
  src: ./templates/            # a directory
  dest: ~/.config/nvim/        # rendered tree root
  mode: "0644"                 # applied to every rendered file
```

- One step, one output line. `changed` when any file changed; the line
  reports the count (`changed  3 of 14`).
- Plan prints one diff per changed file, each headed by its relative path.
- The source tree is walked in sorted order, depth first. Symlinks in the
  source are not followed and are a validation error.
- Files are not required to end in `.j2`; every file is rendered. A `.j2`
  suffix on a source file is stripped from the destination name.
- Nothing under `dest` is removed. This is not a sync; a file deleted from
  the source tree stays on disk. Delete it with `file: {state: absent}`.
- This is the only iteration in the language, and it is not a loop
  construct: no `item`, no scope per file, no user-supplied collection.

### 6.5 `pkg`

Ensure packages are present or absent through one manager.

```yaml
pkg:
  names: [git, curl]           # or name: git
  state: present               # present | absent | latest
  manager: apt                 # apt | pacman | yay | brew | winget; default: first available
  cask: false                  # brew only
  update_cache: false          # apt update / pacman -Sy / brew update before install
```

- Changed when the manager's own query says the package was missing before
  and present after (`dpkg-query`, `pacman -Q`, `brew list`, `winget list`).
  One query for the whole set, one install call for the missing subset.
- `latest` upgrades; changed when the version string differs.
- Plan: lists the packages that would be installed, by actual query.
- `sudo` is not implied. apt and pacman need `sudo: true`; brew must not
  have it. The tool errors on `pkg` + `manager: brew` + `sudo: true`.
- A missing manager binary is an error naming it.
- Repositories, taps, PPAs, AUR helpers: **not** part of `pkg`. They are
  `shell` steps with `unless`. See migration.md for recipes.

### 6.6 `service`

Ensure a service state under systemd (system or user) or launchd.

```yaml
service:
  name: tailscaled
  state: started               # started | stopped | restarted | reloaded
  enabled: true                # optional
  scope: system                # system | user (systemd only)
```

- Changed when `systemctl is-active` / `is-enabled` differs before and after.
  `restarted` is always changed.
- launchd: `launchctl bootstrap`/`bootout` for `enabled`, `kickstart` for
  `restarted`. Best effort; reported honestly.
- Windows: error. Use `shell` with PowerShell.

### 6.7 `assert`

Fail the run if a condition does not hold. Never changes anything.

```yaml
assert:
  command: test -f ~/.ssh/id_ed25519      # exit 0 passes
  # or
  expr: hostname in ["x1", "mainpc"]
  msg: "wrong machine"
```

- Always reports `ok` or `failed`. Plan runs asserts unless `--plan-no-probe`.
- `retry` (§4) composes with `assert`, and is how a readiness gate is
  written: the assert is re-run on failure until it passes or attempts run
  out. `retry: {attempts: 30, delay: 2s}` is a 60-second wait-for-ready.

## 7. Privilege escalation

- `sudo: true` runs the action through `sudo -n` (non-interactive). If sudo
  needs a password, the run fails **before the first step** with a clear
  message, unless `--ask-sudo-pass` was given, in which case the password is
  read once from the terminal and fed via `sudo -S` for every escalated step.
- Environment is passed with `sudo --preserve-env=<the step's env keys>`.
- No `become_user`. Root or the current user. That is the whole matrix.

## 8. Commands

```
provision validate <plan.yml> [--strict]
provision plan     <plan.yml> [--tags t,u] [--skip-tags t] [--var k=v]... [--vars-file f]... [--plan-no-probe] [--no-diff] [--json]
provision apply    <plan.yml> [same as plan] [--ask-sudo-pass] [--verbose] [--stream] [--hide-skipped]
provision facts    [--json]
provision version
```

- `validate`: parse, schema, template syntax, prop schemas, file existence
  for `import`/`use`/`vars_file`/`src`. No commands run. `--strict` also
  rejects `shell`/`cmd` steps with no idempotency gate.
- `plan`: everything `validate` does, plus render templates, evaluate `when`,
  probe `unless`/`creates`/`pkg`/`service`/`file` state, print what would
  change. Runs nothing that mutates. Exit 0 if nothing would change, 2 if
  something would.
- `apply`: plan, then execute. Stops at first failed step. Exit 1 on failure.

Tag selection (Ansible semantics, deliberately):

- `--tags a,b`: run steps carrying `a` or `b`. Untagged steps are skipped.
  Steps tagged `always` run regardless.
- `--skip-tags a`: run everything except steps carrying `a`. It wins over
  `--tags`, `always` included.
- Tag filtering happens **after** `import`/`use` expansion, so a tag on an
  import applies to each imported step. `plan --tags x` must not fail on a
  step the tag excludes; excluded steps are not rendered.
- Filtering therefore runs **before** the step modifiers of §4: a step the
  tags exclude never has its `when` evaluated, never has its fields rendered,
  and so cannot fail on a variable it was never meant to read. The full order
  is: tags → `when` → `unless` → `creates` → action.
- **Structural steps are never filtered.** `vars`, `vars_file`, `import` and
  `use` build the scope every later step reads; skipping them by tag would
  silently unset variables the selected steps depend on. A tag on an `import`
  or `use` still propagates to the steps inside it, which is the point.

Exit codes: `0` ok / nothing to do · `1` failure · `2` plan found changes ·
`3` usage or validation error.

## 9. Output

### 9.1 TTY

One line per step, updated in place while running (spinner), then frozen:

```
  ✓ Install zsh                                    ok        0.4s
  ~ Deploy .zshrc                                  changed   0.0s
  - Generate SSH identity                          skipped   creates exists
  ? Add neovim PPA                                 unknown   1.2s
  ✗ Enable multilib                                FAILED    0.1s
    │ sed: can't read /etc/pacman.conf: No such file or directory
    │ (exit 2 · stderr, last 20 lines · --verbose for all)

  x1.yml · 5 steps · 1 changed · 1 ok · 1 skipped · 1 unknown · 1 failed · 1.7s
```

- Glyphs and colors: `✓` green ok, `~` yellow changed, `-` dim skipped, `?`
  magenta unknown, `✗` red failed. `NO_COLOR` and `--color=never` honored.
- Nested `import`/`use` shown as a dim header line with the file name;
  steps indented one level. Depth capped at display; execution is flat.
- Skipped steps collapse to one dim line each; `--hide-skipped` drops them
  and the summary still counts them.
- Plan mode uses `would change` / `would run` in place of `changed`, and
  prints diffs under the line, indented, colored.
- Retries render as `attempt 2/3` on the line while running.

### 9.2 Non-TTY

Same lines, no spinner, no color, no in-place updates. Failure output is
printed the same way. Suitable for CI logs.

### 9.3 `--json`

One JSON object per line on stdout, human output goes to stderr:

```json
{"event":"step","index":3,"name":"Deploy .zshrc","status":"changed","duration_ms":12,"file":"components/zsh/index.yml","line":41,"diff":"..."}
{"event":"summary","changed":1,"ok":1,"skipped":1,"unknown":0,"failed":0,"duration_ms":1700}
```

## 10. Edge cases

| Case | Behavior |
|---|---|
| Step has two action keys | Validation error with file:line |
| `import` cycle | Error naming the cycle |
| Template renders to empty `name` | Falls back to action + first arg |
| `unless` command not found | Error, not "run the step" |
| `creates` path contains `~` or template | Expanded, rendered, then checked |
| `sudo: true` on macOS with `pkg: brew` | Validation error |
| `retry` on a step that changed on attempt 2 | Reported changed, `attempt 2/3` in output |
| Timeout kills a `shell` with children | Process group killed, not just the shell |
| Plan probes with `--plan-no-probe` | `unless`/`assert`/`pkg` queries skipped, reported `would run (unprobed)` |
| `when` reads a `register` during `plan` | The step that registers it has not run, so the name holds a placeholder (`rc: 0`, empty output, `changed: false`) and the reader is reported `would run (unprobed)` rather than skipped. Plan does not claim a verdict it cannot have (D11) |
| Non-UTF-8 in stdout | Lossy display, exact bytes in `register` |
| Ctrl-C mid-step | Current step killed, summary printed with `interrupted`, exit 130 |
| Windows path in `dest` | Accepted; `~` expands to `%USERPROFILE%` |
| Two `vars_file` set the same key | Later wins, `--explain-var k` shows the chain |

## 11. Validation of this spec

Before code: walk every step type in `~/dotfiles` (64 files, ~950 steps)
against §3–§6 and record each construct that has no mapping. The migration
document must list all of them. Zero unmapped constructs, or an explicit
decision to rewrite each, is the gate for starting Phase 0.

During build: each action ships with an integration test that runs it
twice on a scratch filesystem or container and asserts `changed` on the
first run and `ok` on the second. That test is the definition of
"idempotent" for this project.

After build: `provision plan` on all five machine plans reports zero
changes on a machine mooncake has already converged, except for steps the
migration document predicts.
