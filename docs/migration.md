# Migrating `~/dotfiles` from mooncake to provision

Status: v1.2 · 2026-09-08 · executed against `~/dotfiles` at commit `9548bbd`
on branch `provision`; §7 step 1 is done
(58 YAML files, 371 steps, 27 `.j2` templates, 18 components, 5 machines).

Counts below are the audited ones from [audit.md](audit.md), which closed the
spec §11 gate. The earlier hand counts in draft v0.1 were taken before the
dex drop and are corrected here.

## 1. Principle

The dotfiles stay YAML, stay Jinja2, keep their layout
(`machines/`, `platforms/`, `components/`, `shared/`). Most steps change
only their action key. The migration is a mechanical rewrite plus a short
list of hand edits, done once per file, verified by `provision plan`
against a machine mooncake has already converged.

## 2. Key mapping

### Structural

| mooncake | provision | Notes |
|---|---|---|
| `vars:` | `vars:` | unchanged |
| `vars.load: path` | `vars_file: path` | rename |
| `import: path` | `import: path` | unchanged; `when`/`tags` on it still apply to all imported steps |
| `use: path` + `props:` | `use: path` + `props:` | unchanged; component files keep `props:` + `steps:` |
| `tasks.yml` | delete | task runner is a non-goal; see §6 |

### Modifiers

| mooncake | provision | Notes |
|---|---|---|
| `as_user: root` | `sudo: true` | 77 sites, mechanical |
| `unless_command: X` | `unless: X` | 39 sites, mechanical |
| `creates:` | `creates:` | unchanged |
| `timeout:` | `timeout:` | unchanged, same duration syntax |
| `retry: {attempts, delay}` | `retry: {attempts, delay}` | unchanged |
| `as: name` | `register: name` | 3 sites |
| `changed_when` / `failed_when` | same | `result` stays in scope |
| `on_change:` | — | **zero sites**; dex took the only one. Nothing to migrate. |
| `for_each:` (2) | a literal `for` loop inside one `shell` | loops are a non-goal; see §3.6 |
| `for_each_file:` (1) | `template` directory mode (spec §6.4) | see §3.7 |
| `env:` | `env:` | unchanged |
| `tags:` | `tags:` | unchanged; **semantics change**, see §5 |
| `-K` / `--ask-become-pass` | `--ask-sudo-pass` | |

### Actions

| mooncake | provision | Notes |
|---|---|---|
| `shell: "..."` | `shell: "..."` | unchanged |
| `shell: {cmd, interpreter: powershell, run_as_admin: true}` | `shell: {script, interpreter: powershell}` | `cmd` → `script`; drop `run_as_admin`; run from an elevated prompt (already the documented practice) |
| `shell: {cmd, creates}` (2) | `shell: …` with `creates` as a step modifier | lift `creates` one level out of the action body |
| `cmd: {argv: [...]}` | `cmd: [...]` | flatten |
| `file.write: {path, content, state: directory, mode, owner, group}` | `file: {path, content, state: dir, mode, owner, group}` | `directory` → `dir`; 52 sites. `group` is now a spec §6.3 field (2 sites) |
| `file.copy: {src, dest}` | `file: {path: dest, src, state: file}` | 3 sites |
| `file.download` (1) | `shell: curl -fsSL URL -o PATH` + `creates: PATH` | |
| `file.template: {src, dest}` | `template: {src, dest}` | 32 sites, rename |
| `pkg: {name/names, state, manager, cask, update_cache}` | `pkg:` same | unchanged; `manager: yay` supported |
| `pkg.upgrade: {}` (1) | **delete** | a full distro upgrade is a choice, not a state. Decided 2026-09-08; upgrade by hand or from the justfile (§6) |
| `pkg.repo` brew (1 site, 4 taps) | `shell: brew tap X` + `unless: brew tap \| grep -qx X` | §3.1, collapsed into the one loop of §3.6 |
| `pkg.repo` apt (3) | keyring `shell` + `file` DEB822 source + `cmd: [apt-get, update]` | **three steps, not one.** §3.11. Not PPAs — §3.3 does not cover them |
| `os.service` (10) | `service: {name, state, enabled, scope}` | `daemon_reload: true` (1 site) becomes a preceding `cmd: [systemctl, daemon-reload]`. On launchd the same shape applies for a different reason: the step that places a plist must bootstrap it in a following `cmd`, because `kickstart` cannot load one that was never loaded. Zero launchd service sites in the fleet today |
| `os.systemd` (5) — `unit`/`service`/`install` blocks | `template` + `cmd: [systemctl, daemon-reload]` + `service` | **writes a unit file**; `service` does not. Recipe in §3.8. `reload_on_change` becomes `register` + `when` |
| `os.user` (2) | `shell: chsh` + `unless` | recipe in §3.5 |
| `stat` (2) + `pip` (2) | delete | only in `components/nvim/python_venv.yml`, which is deleted; see §4 |
| `log` (2) | `shell: echo` + `changed_when: false`, or move to a README | recipe in §3.9 |
| `text.replace` (3) | `shell` with sed + `unless: grep -q` | recipe below |
| `text.line` (1) | `shell: grep -qxF LINE FILE \|\| echo LINE >> FILE` + `unless` | |
| `git.clone` (2) | `shell: git clone URL DEST` + `creates: DEST` | |
| `assert: {command: {cmd}}` | `assert: {command: "..."}` | flatten, 24 sites |
| `wait.http` (1) | `assert: {command: curl -fsS …}` + `retry` | the one site is in `moongit`, which is **kept**, not `fleet-peer`. Recipe in §3.10 |
| `container.image` | — | **zero sites**; dex took both. Nothing to migrate. |
| `windows.scheduled_task` (4), `windows.firewall_rule` (3), `windows.hyperv_firewall_rule` (3) | `shell` PowerShell with an `if (-not (Get-…))` guard and `changed_when` | the bootstrap already does this pattern for every other step |

### Facts

| mooncake | provision |
|---|---|
| `os`, `arch`, `hostname`, `username` | same names |
| `apt_available`, `pacman_available`, `brew_available` | same names; add `yay_available`, `winget_available` |
| `is_wsl` (set as a var per machine) | now a fact, computed; delete the `vars:` lines that set it |

## 3. Recipes for dropped actions

Each numbered recipe is the agreed replacement for one mooncake action.
Section numbers match the audit ([audit.md](audit.md) §3) where they overlap.

### 3.1 Brew tap

```yaml
- name: Tap hashicorp
  shell: brew tap hashicorp/tap
  unless: brew tap | grep -qx hashicorp/tap
```

### 3.2 Uncomment a block in pacman.conf

```yaml
- name: Enable multilib
  shell: sed -i 's/^#\[multilib\]/[multilib]/; /^\[multilib\]/{n;s/^#Include/Include/}' /etc/pacman.conf
  unless: grep -qx '\[multilib\]' /etc/pacman.conf
  sudo: true
```

### 3.3 Apt PPA

```yaml
- name: Add neovim PPA
  shell: add-apt-repository -y ppa:neovim-ppa/unstable && apt-get update
  unless: ls /etc/apt/sources.list.d | grep -q neovim-ppa
  sudo: true
```

### 3.4 Windows firewall rule — honest `changed`

```yaml
- name: Open agentd port
  shell:
    interpreter: powershell
    script: |
      if (Get-NetFirewallRule -DisplayName "agentd" -ErrorAction SilentlyContinue) { exit 3 }
      New-NetFirewallRule -DisplayName "agentd" -Direction Inbound -LocalPort 7878 -Protocol TCP -Action Allow
  failed_when: result.rc not in [0, 3]
  changed_when: result.rc == 0
```

The same shape covers `windows.scheduled_task` (`Get-ScheduledTask`) and
`windows.hyperv_firewall_rule` (`Get-NetFirewallHyperVRule`).

### 3.5 `os.user` — set the login shell

Replaces `platforms/arch/index.yml:130` and `components/zsh/index.yml:105`.
The `unless` differs by platform because macOS keeps the shell in Directory
Services, not `/etc/passwd`:

```yaml
- name: Default shell (linux)
  shell: chsh -s "{{ zsh_shell_path }}" "{{ username }}"
  unless: 'test "$(getent passwd {{ username }} | cut -d: -f7)" = "{{ zsh_shell_path }}"'
  sudo: true
  when: os == "linux"

- name: Default shell (macos)
  shell: chsh -s "{{ zsh_shell_path }}" "{{ username }}"
  unless: "test \"$(dscl . -read /Users/{{ username }} UserShell | awk '{print $2}')\" = \"{{ zsh_shell_path }}\""
  sudo: true
  when: os == "darwin"
```

`components/zsh/index.yml:114` already asserts the macOS form; that assert
stays and now verifies a step it sits next to.

### 3.6 `for_each` over brew taps

`platforms/macos/packages.yml:141,151` iterate the same 4-element `taps`
var — once for `pkg.repo`, once for `brew trust`. With `pkg.repo` gone
(§3.1), both collapse into one shell step over a literal list. No loop
construct, and the `taps` var (whose `name` field existed only to feed
`pkg.repo`) is deleted:

```yaml
- name: Tap and trust brew taps
  shell: |
    set -euo pipefail
    for t in hashicorp/tap borkdude/brew wata727/tflint wagoodman/dive; do
      brew tap | grep -qx "$t" || brew tap "$t"
      brew trust "$t"
    done
  changed_when: false
  timeout: 5m
```

### 3.7 `for_each_file` — the nvim template tree

`components/nvim/index.yml:65` renders 14 files across 3 levels. This is the
one construct that earned a spec change: `template` now takes a directory as
`src` (spec §6.4). One step replaces the loop:

```yaml
- name: Deploy nvim config
  template:
    src: ./templates/
    dest: "{{ config_path }}/"
    mode: "0644"
  tags: [nvim, config]
```

The `when: not item.IsDir` guard goes away — directory mode only renders
files. Note this is **not** a sync: a template deleted from the source tree
leaves its rendered file on disk.

### 3.8 `os.systemd` — unit file, reload, service

Replaces the 5 sites in `machines/x1/thermal.yml` and
`components/moongit/index.yml`. The unit body moves into a `.j2` file next
to the component, so `plan` shows a real diff of it — which `os.systemd`
never did:

```yaml
- name: intel-rapl-cap unit
  template:
    src: ./units/intel-rapl-cap.service.j2
    dest: /etc/systemd/system/intel-rapl-cap.service
    mode: "0644"
  sudo: true
  register: rapl_unit

- name: Reload systemd
  cmd: [systemctl, daemon-reload]
  when: rapl_unit.changed
  sudo: true

- name: intel-rapl-cap
  service: { name: intel-rapl-cap.service, state: started, enabled: true }
  sudo: true
```

- `scope: user` units render to `~/.config/systemd/user/<name>` with no
  `sudo`, and the reload is `[systemctl, --user, daemon-reload]`.
- `started: true` is `state: started`. `enabled: true` is unchanged.
- `reload_on_change: true` (moongit) becomes a fourth step:
  `service: {state: restarted}` with `when: <unit>.changed`. This is the
  same `register` + `when` substitution D5 prescribes for `on_change`.
- `os.service`'s `daemon_reload: true` (`platforms/windows/index.yml:89`)
  uses the middle step alone, unconditionally.

### 3.9 `log`

```yaml
- name: "Note: mint an API token to use /api/*"
  shell: echo "moongit up on {{ props.moongit_addr }}. If /api/* 401s, run: {{ moongit_bin_path }} token create <name>"
  changed_when: false
```

`shell` + `changed_when: false` is a `log` action and costs nothing. The
other site (`platforms/windows/bootstrap.yml:352`, a next-steps banner) is
operator documentation, not state — it moves to
`platforms/windows/README.md` and is deleted from the run.

### 3.10 `wait.http` — readiness gate

`components/moongit/index.yml:237` polls `/healthz` before the steps that
mint a token against the daemon. `retry` composes with `assert` (spec §6.7):

```yaml
- name: Wait for moongit to become ready
  assert:
    command: curl -fsS -o /dev/null http://127.0.0.1{{ props.moongit_addr }}/healthz
    msg: "moongit did not become ready"
  retry: { attempts: 30, delay: 2s }
```

30 attempts × 2s is the 60s timeout the original declared.

### 3.11 `pkg.repo` apt — keyring, DEB822 source, update

Replaces `components/terraform/index.yml:13`,
`components/google-cloud/index.yml:10` and
`platforms/windows/index.yml:127` (tailscale). mooncake's `pkg.repo` apt
driver did keyring fetch, source write and `apt-get update` atomically in
one step; provision has no such action and is not getting one (decided
2026-09-08 — the heaviest action in the language for three sites). It
expands to three steps, which is also the first time the update is
visibly gated on the source actually changing:

```yaml
- name: Tailscale apt keyring
  shell: curl -fsSL https://pkgs.tailscale.com/stable/ubuntu/{{ ubuntu_codename }}.noarmor.gpg -o /etc/apt/keyrings/tailscale.gpg
  creates: /etc/apt/keyrings/tailscale.gpg
  sudo: true

- name: Tailscale apt source
  file:
    path: /etc/apt/sources.list.d/tailscale.sources
    content: |
      Types: deb
      URIs: https://pkgs.tailscale.com/stable/ubuntu
      Suites: {{ ubuntu_codename }}
      Components: main
      Signed-By: /etc/apt/keyrings/tailscale.gpg
    mode: "0644"
  sudo: true
  register: tailscale_repo

- name: Refresh apt after the Tailscale source changed
  cmd: [apt-get, update]
  when: tailscale_repo.changed
  sudo: true
```

- `/etc/apt/keyrings` must exist; on all three targets it ships with
  `apt`. The keyring step is gated by `creates`, so a rotated key needs
  the file deleted — same posture mooncake had with `gpg_check: false`.
- `gpg_check: false` on all three sites means no fingerprint was ever
  pinned. The rewrite does not change that; pinning is a separate call.
- The `.sources` (DEB822) form is deliberate and matches what `pkg.repo`
  wrote, so the `Signed-By` conflict documented at
  `platforms/windows/index.yml:111` stays resolved.

## 4. Hand edits, by file

Complete. The spec §11 gate ([audit.md](audit.md)) walked all 371 steps and
extended this list; every construct in `~/dotfiles` now has a mapping.

| File | Edit |
|---|---|
| `mooncake.yml` (root dispatcher) | rename to `provision.yml`; keeps `import` + `when: hostname == …` |
| `machines/*/index.yml` | delete the `vars: is_wsl:` block (fact now); `as_user` → `sudo`; assert flatten |
| `shared/bootstrap.yml` | delete the systemd-linger step's agentd justification comment; delete the sudoers step's mooncake-specific comment; keep the steps |
| `components/fleet-peer/` | **keep, keys only.** Corrected 2026-09-08: draft v1.0 said delete, premised on dropping mooncake. mooncake stays in use, and every step here is a `shell`/`file`/`assert` that migrates mechanically — including `mooncake agentd bootstrap`, which is just a command. provision has no fleet feature; it does not need one to provision a mooncake peer |
| `components/mooncake/` | **keep, keys only.** Corrected 2026-09-08: the component installs nothing — its one step builds the `mooncake-ci:latest` image moongit runs mooncake's repo CI in, on main_pc. mooncake stays a tool in use. No `components/provision/`: there is no release URL yet |
| `components/moongit/` | keep; `os.systemd` → §3.8 recipe with `scope: user` + a `reload_on_change` restart step; `wait.http` → §3.10; `log` → §3.9 |
| `components/nvim/python_venv.yml` | **delete**, and its `import`. Sole user of `stat` and `pip`; its `when:` on line 14 tests `python3_venv_path` (a path, always truthy) instead of `python3_venv_present`, so it has been dead since it was written |
| `components/nvim/index.yml` | `for_each_file` → §3.7 one-step directory template; delete the `python_venv.yml` import |
| `machines/x1/thermal.yml` | 4 × `os.systemd` → §3.8; add `units/*.service.j2` next to it; **2 × `text.replace`** on `arch.conf` → §3.2 sed shape (missed by draft v1.0, which listed only the arch site) |
| `platforms/macos/packages.yml` | `pkg.repo` + `for_each` taps → §3.6 single shell step; delete the `taps` var; `shell` xcode/rosetta/brew steps unchanged |
| `components/terraform/index.yml`, `components/google-cloud/index.yml` | `pkg.repo` apt → §3.11 three-step form |
| `platforms/arch/index.yml` | `text.replace` multilib → §3.2; `text.line` → recipe; `os.user` → §3.5; yay build step unchanged; **`pkg: {upgrade: true}` → `update_cache: true`** — the step was doing a `pacman -Syu` *and* the database refresh every other `pkg` step installs against. The upgrade goes (same call as `pkg.upgrade`); the refresh stays |
| `platforms/windows/index.yml` | `os.service` `daemon_reload: true` → a preceding `cmd: [systemctl, daemon-reload]` (§3.8); tailscale `pkg.repo` → §3.11 |
| `components/zsh/index.yml` | `os.user` → §3.5; `git.clone` → `shell` + `creates` |
| `components/tmux/index.yml`, `components/claude/index.yml` | `file.copy` → `file: {src, state: file}`; `git.clone` → `shell` + `creates` |
| `components/nvim/win32yank.yml` | `file.download` → `shell: curl` + `creates` |
| `components/nvim/zk_config.toml.j2`, `zk_template_default.md.j2`, `components/zsh/templates/.zshrc.j2` | `{% verbatim %}`/`{% endverbatim %}` → `{% raw %}`/`{% endraw %}` (3 files, 3 blocks) — Jinja2's tag; see §5 |
| `platforms/windows/packages.yml` | `pkg.upgrade` → **deleted** (decided 2026-09-08); PPA → §3.3 |
| `platforms/windows/bootstrap.yml` | drop `run_as_admin`; `shell.cmd` → `shell.script`; typed windows actions → §3.4 PowerShell recipes; the closing `log` banner moves to `platforms/windows/README.md` |
| `shared/bootstrap.yml` | sudoers drop-in keeps `owner` **and** `group` (now a spec §6.3 field) |
| `machines/*/vars.yml` | **keep all of them.** Corrected 2026-09-08: `wsl_agentd_port`, `windows_agentd_port` and `fleet_*` feed the mooncake agentd and fleet, both of which stay. `shared/variables.yml` already defaults `windows_prevent_sleep` and `wsl_networking_mode`, so no machine needs to redeclare them |
| `components/palette/index.yml` | **`use` → `import`**, and the `props:` block goes. The component's whole job is to put `palette.*` / `editor.*` into the CALLER's scope; under mooncake that worked only because `vars.load` mutated the global table. provision's `use` builds a child scope that does not leak back, so every consumer template (tmux, alacritty, hyprland, waybar, wofi, dunst, hyprlock, zsh) failed with `undefined variable palette`. `import` shares scope by design. The `enum:` prop goes with the block — a bogus variant is now a positioned "no such vars file" error |
| `components/nvim/copilot.yml` | `when: false` → `when: "false"`. A YAML boolean is not a Jinja2 expression. The file is one permanently-disabled step; kept as parked work rather than deleted |
| `components/clojure/` | `{{ cognitec_dev_tools_password }}` was **defined nowhere**, so mooncake rendered an empty `<password>` into `~/.m2/settings.xml` on every apply of four machines. Now `{{ env.COGNITECT_DEV_TOOLS_PASSWORD }}` with `when: env.COGNITECT_DEV_TOOLS_PASSWORD is defined`, so the file is written only when the secret is actually present |
| `platforms/windows/bootstrap.yml` | also gains `- vars_file: ../../shared/variables.yml` as its first step. mooncake needed a second `-v` flag the operator had to remember; the plan is now self-contained apart from the machine's own vars. `{{ user_home }}` → the `home` fact, deleting an undeclared variable |
| `tasks.yml`, `mgitci.yml` | see §6 |

Expected `plan` diffs on an already-converged machine after migration:

- Windows bootstrap steps that were `changed: true` forever now report
  `ok` or `skipped`. Good.
- Any step that was gated only by mooncake's shell `changed` heuristic now
  reports `unknown` until it gets a gate. Each is a hand edit. Target zero.

## 5. Behavior changes to know about

- **Tags.** `--tags x` now runs *only* `x`-tagged steps (plus `always`).
  Under mooncake it ran untagged steps too. Audit the `tags:` on
  `shared/bootstrap.yml` imports: anything that must always run gets
  `always`.
- **Strict templates.** An undefined variable fails `validate`. Expect a
  handful in rarely-run branches (`when: false` blocks). Fix or delete.
- **`sudo -n` preflight.** `--ask-sudo-pass` replaces `-K`, and it is
  needed on a machine that has never been applied to. **Corrected
  2026-09-08:** this used to say "on mac and x1, without NOPASSWD", which
  is wrong — `shared/bootstrap.yml` installs
  `/etc/sudoers.d/<user>-nopasswd` with `NOPASSWD: ALL` on every linux and
  darwin host, and all five machine plans import it. mooncake's own
  bootstrap writes the identical file, so the machines it converged already
  have it. The three machine headers that prescribed the flag forever were
  fixed in dotfiles `6ff625f`.
- **No `changed` for gate-less shell.** It shows as `unknown`, in magenta.
  This is a feature.
- **Template directory mode is not a sync.** `for_each_file` re-rendered the
  tree each run and so did nothing on deletion either, but the new one-step
  form makes it look like a directory is being managed. It is not: removing
  a template leaves its rendered file behind. Delete it explicitly.
- **`{% verbatim %}` is not Jinja2.** It is Twig's tag; Jinja2 and minijinja
  spell it `{% raw %}`. mooncake's engine accepted `verbatim`, so three
  templates use it. One-word rewrite in each, listed in §4. Nothing else in
  the 27 templates changes.
- **Booleans render `True`, not `true`.** Jinja2 is Python, and provision
  renders as Jinja2 does. No current template interpolates a boolean, so this
  is inert today; a future one needs `{{ flag | to_json }}` to get `true`.
- **A field that is one `{{ … }}` keeps its type.** `names: "{{ apps }}"` is
  the list, as it was under mooncake. Spec §3.4 now states the rule.
- **One output line for 14 files.** The nvim config deploy was 14 lines of
  output; it is now one, reading `changed  3 of 14`. Use `plan` for the
  per-file diffs.
- **`use` does not leak, and one component depended on that.** See the
  `components/palette/` row in §4. Anything whose purpose is to publish
  variables to its caller is an `import`, not a `use`.
- **`validate` is not platform-scoped.** mooncake compiled each step
  against the *current* platform before evaluating `when:`, so validating
  `main_pc.yml` on a mac died on `action 'os.systemd' is not supported on
  platform 'darwin'` — which is why `tasks.yml`'s `ci` task checked each
  config only on the host that could run it. provision validates all five
  machine plans from any one box, so `just ci` is a complete gate
  everywhere.
- **No `--keep-going`.** mooncake's flag finished every step it could and
  listed failures at the end, which is what made a first apply on a bare
  machine survivable. provision has no equivalent yet; the spec should
  settle one before §7 step 2.

## 6. Things that move out of dotfiles entirely

- `tasks.yml` (per-machine apply tasks, backup, ci): a five-line
  `justfile`. **Done on the `provision` branch** (decided 2026-09-08):
  `tasks.yml` cannot be migrated — task running is a non-goal — so
  leaving it would only mean excluding a file from `validate`.

  ```
  x1:        mkdir -p ~/.local/state/provision
             provision apply x1.yml --json >> ~/.local/state/provision/x1.jsonl
  plan m:    provision plan {{m}}.yml
  ci:        for m in main_pc mini_pc x1 mac work_mac; do provision plan --plan-no-probe $m.yml; done
  ```

  Each apply recipe appends a JSON run log (D9). `--json` moves the human
  output to stderr, so the terminal still shows progress while the shell
  keeps the record; there is no `--log` flag and there will not be one.

  `just` is a fleet dependency these recipes introduced, and the owner
  ruled on it on 2026-09-08: it goes in all three platform package lists
  (`just` on apt, pacman and brew alike). Until a machine has been applied
  since, run the recipe's command by hand — it is one line.

- `mgitci.yml`: replace the `mooncake validate` / `mooncake plan --no-inspect`
  steps with `provision validate` / `provision plan --plan-no-probe`. The CI
  image carries the `provision` binary **as well as** mooncake, not instead
  of it: moongitd execs every CI step as `mooncake step '<yaml>'` inside the
  container, so the image has to carry mooncake whatever applies the
  dotfiles. `components/provision` builds `provision-ci:latest` FROM
  `mooncake-ci:latest` plus the binary, sha-gated on the binary's hash and
  the base image id.

  **Landed on the branch 2026-09-08** (dotfiles `367624c` for the component,
  `91415fc` for the switch), superseding the earlier "not on this branch"
  ruling. The hazard it named is real and unchanged, so the switch is its
  own last commit and says so in its message: **CI is red until the image
  exists on main_pc**, because CI cannot pull (dotfiles#8) and building the
  image is a real apply — §7 step 6, the owner's.

- The nine repos with `tasks.yml` (`dex`, `moongit`, `moongit-*`, `cry-aye`)
  and the `go-quality` presets are a separate migration to `just`, outside
  this project's scope. They keep working on mooncake until then.

## 7. Order

1. Phase 0 gate: mechanical rewrite of keys across all 58 YAML files in a
   `provision` branch of dotfiles. `provision validate` green on all five
   machine plans, and every `.j2` renders under `--plan-no-probe`.
   `mgitci.yml` is out of scope (§6) and `tasks.yml` is deleted.
2. main_pc WSL first. It is the box the work was done on, so a failure is
   diagnosable in the same session that caused it, and it is the CI server:
   `provision-ci:latest` cannot be built anywhere else. `plan`, fix
   `unknown`s, `apply`, `plan` again shows nothing.
3. main_pc Windows host. Done alongside the WSL side because the two halves
   share `.wslconfig` and the firewall rules, and the host is reachable from
   the WSL side without leaving the machine.
4. mini_pc, both sides.
5. mac and work_mac.
6. x1 last. It is the only always-with-me machine and the usual controller;
   breaking it strands the fleet. Everything else has proved the tool first.
7. Merge the branch. **mooncake stays installed** — ruled 2026-09-08: it is
   a tool, not a dependency. It is the MCP server, `components/fleet-peer`
   runs its agentd, and `provision-ci:latest` is built FROM `mooncake-ci`
   because moongitd execs every CI step as `mooncake step '<yaml>'`. What
   ends here is using it to provision, not using it.

## 8. What the rewrite found

Every one of these had been live in `~/dotfiles` and silent under mooncake.
None was a migration error; strict undefined and positioned validation are
what surfaced them.

| Where | Latent bug |
|---|---|
| `components/clojure/templates/settings.xml` | `~/.m2/settings.xml` written with an empty `<password>` on x1, mac, main_pc and mini_pc since the file was added — the variable was never defined anywhere |
| `components/nvim/python_venv.yml` | dead since written: its `when:` tested `python3_venv_path`, a path (always truthy), instead of `python3_venv_present`. Also never imported by anything. Deleted |
| `components/nvim/index.yml` | the `python_libs` / `python3_libs` vars had no reader once `python_venv.yml` went |
| `platforms/arch/index.yml` | one step silently did both a full `pacman -Syu` and the database refresh every later `pkg` step depends on |
| `platforms/windows/bootstrap.yml` | rendered `{{ user_home }}`, declared in no file in the repo |
| `machines/main_pc/vars.yml` | `windows_prevent_sleep` undefined → mooncake read it falsy, so main_pc silently took the *remove-keepalive* branch. Left as-is (`shared/variables.yml` defaults it false); flipping it is its own change |
| 3 `.j2` templates | `{% verbatim %}`, which is Twig's tag. mooncake's engine accepted it; Jinja2 and minijinja spell it `{% raw %}` |

The gate also found a hole in provision itself: `deny_unknown_keys` was
applied to step keys but not to action bodies, so `shell: {cmd: …}` and
`service: {daemon_reload: true}` passed validation silently. Fixed —
`model::action_body_keys` now checks every action's long form against
spec §6, and it is what caught the `pkg: {upgrade: true}` above.

## 9. Status

`§7 step 1 is complete` on the `provision` branch of `~/dotfiles`:

```
x1        ok    165 steps · 144 would run · 21 skipped
mac       ok    116 steps ·  93 would run · 23 skipped
work_mac  ok    103 steps ·  83 would run · 20 skipped
main_pc   ok    153 steps · 132 would run · 21 skipped
mini_pc   ok    122 steps · 103 would run · 19 skipped

platforms/windows/bootstrap.yml (standalone, --vars-file machines/<m>/vars.yml)
          ok     18 steps ·  16 would run ·  2 skipped
```

`validate --strict` still reports ungated `shell` steps — 12 on x1, 8 on
main_pc, 7 each on mac, work_mac and mini_pc. That is the expected state
named at the end of §4: each is a hand edit, and they are the work of §7
step 2, per machine, alongside the first real `apply`.
