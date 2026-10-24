# ADR-017 — The agent asks for the run, the person confirms it in a window

Status: **accepted** · 23 Oct 2026 · extends [ADR-004](ADR-004-approval.md)

## Context

With ADR-004, Claude designs the scenario and the person has to go to their terminal to approve and run every time. That breaks the most useful cycle: run, compare against the baseline, touch the API's code and run again. The person wants the agent to be able to launch runs while it works, without losing what principle 1 asks: **no load goes out without a person**.

Today `crabfall run` doesn't require a terminal (ADR-004: an approval made by a person can be run from a script); the only thing stopping the agent is the hook. And approvals don't expire. If `run` were simply unlocked, the agent could repeat runs without anyone confirming anything.

## Decision

### 1. `crabfall run --ask`

- **Without a terminal** (an agent launched it): Crabfall relaunches itself in a **new console** (`CREATE_NEW_CONSOLE` on Windows) with `--window <file>` and waits. The window shows the plan, asks for confirmation, **runs the load there** with its progress, and at the end leaves in the file the exit code and where the results are. The agent's process prints that and exits with the same code.
- **In a terminal** (a person typed `--ask`): it asks right there.
- Before opening the window, the plan is built and validated and the `limits` are applied: errors reach the agent without bothering the person.

Running the load in the window gives an **emergency brake** that doesn't depend on the agent: Ctrl+C stops it in order, as in `run` (ADR-015). At the end, the window shows the summary and closes on its own after 30 s (or with Enter).

### 2. What is asked

- **Without a valid approval** (first time, or the plan changed): the risk summary with the diff, and typing the host, as in `approve`. It signs the approval.
- **With a valid approval**: the summary and a short answer, `y`/`s` for this run. The agent can't type in the window, so that's enough; typing the host on every run is tiring and ends in typing without reading.
- In both cases, the person can answer with **minutes (1 to 120)**: it allows the same plan without asking for that long. If nobody answers in 5 min, it's cancelled.

### 3. The time-limited allowance

`.crabfall/approvals/<scenario>.<environment>.allow.json`, signed with the same HMAC key over `crabfall-allowance/<format>`, scenario, environment, the approved plan's hash and the expiry time.

- It holds only for that hash: any change to the plan invalidates it and asks again with the diff.
- It's by time, not by number of runs: copying an old file gives back no runs, because the signed time doesn't change.
- During the allowance, the window opens anyway: the person sees every run and can stop it.
- It's revoked by deleting the file.

### 4. The window defends itself

`--window` only works if the console is **new and visible**: the process is the only one in it (`GetConsoleProcessList`) and the window is visible (`IsWindowVisible`). In the agent's console or in a shared terminal, it refuses with code 3 without asking anything. `--ask` and `--window` together are a usage error.

### 5. Hook and skill

The hook lets `crabfall run ... --ask` through and still blocks `run` without `--ask`, `run --window`, `approve` and the TUI. The skill tells Claude to use `--ask` in the background, to wait for the person, and to read the result with `crabfall findings --json` and `crabfall compare --baseline --json`. Only the person moves the baseline.

### 6. Other systems

**Linux** (24 Oct 2026), with a desktop (`DISPLAY` or `WAYLAND_DISPLAY`; WSL2 with WSLg works):

- The window opens in `$TERMINAL` or in the first one available of `x-terminal-emulator`, gnome-terminal, ptyxis, konsole, xfce4-terminal, mate-terminal, kitty, alacritty, wezterm, foot and xterm, each with its own way of receiving the command.
- Some return at once (gnome-terminal opens the window from its server), so `delegate` doesn't wait for the terminal: the window leaves its pid next to the answer file, and the agent waits for the answer or for that process to stop existing (the window was closed). If the pid doesn't appear in 20 s, the terminal didn't open the window.
- The answer is written aside and renamed, so it isn't read half-written.
- The equivalent of §4: input and output on a terminal, the process leads its own session (the terminal launched it, not a shell) and its parent is a known terminal, or the one in `$TERMINAL` if it isn't a shell or something like `script` or `tmux`, which give a terminal without showing it. Whether the window is visible can't be checked in general.
- The key lives in the Secret Service (GNOME Keyring or KWallet), which has to be running and unlocked.

**macOS** (Terminal through `osascript`), later: `--ask` without a terminal refuses and asks the person to run the command. Without a desktop (SSH, Claude Code on the web) there's nowhere to open the window: the same refusal.

## What doesn't change: the malicious agent

ADR-004 doesn't protect against an agent acting deliberately maliciously with the user's permissions, and neither does this. We checked:

- **Synthetic keystrokes.** UIPI only prevents injecting input into processes of *higher* integrity; between processes of the same user and level, `SendInput` works ([SendInput](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-sendinput)). The agent can type into the window, or launch `crabfall` under its own ConPTY. The check in §4 raises the cost, but doesn't prevent it.
- **The key.** Any process of the user reads that user's Credential Manager ([CredRead](https://learn.microsoft.com/en-us/windows/win32/api/wincred/nf-wincred-credreadw)): it can sign approvals and allowances.
- **Simpler still:** patching the binary, or generating load without Crabfall (a `curl` loop).

Any check inside a process the agent controls can be bypassed. Only what stays **out of its reach** holds. Options, from lowest to highest cost:

1. **Sandboxing the agent** (recommended in the short term, with no new code). Claude Code's sandbox (macOS, Linux and WSL2; there is none on native Windows) restricts the network with an allowlist of hosts and can deny reads (`denyRead` over the configuration and the keyring), with `failIfUnavailable` so it doesn't silently fall back to "no sandbox" ([sandboxing](https://code.claude.com/docs/en/sandboxing)). If the agent can't reach the target or the key, only `crabfall`, launched outside the sandbox, generates load. A devcontainer or a VM with restricted egress achieves the same.
2. **The target requires a short-lived token** (the most robust). Caddy (`forward_auth`) only accepts traffic with a token issued after a confirmation outside the agent's machine: a FIDO2 key (a physical touch, an asymmetric key; the machine only stores the public one) or a phone. It only works for targets you own, like `market-api`, and the verifier has to live on another host or user.
3. **A broker running as another system user** that holds the key and is the only one with network access to the target. It holds against an agent without admin rights, but it's costly to install and not cross-platform. UAC doesn't work as a boundary: Microsoft doesn't consider it a security boundary.

What is **not** enough on its own: Windows Hello or Touch ID as a "yes/no" (the result is a boolean the agent can bypass by patching the binary), TOTP with the secret on the same machine, or MCP elicitation (the client itself mediates it).

## Consequences

- The agent can iterate (run, compare, change the code) and the person only confirms. With the time-limited allowance, not even that, but they still see every run and can stop it.
- A window that pops up can end up behind another: the console beeps (bell) and flashes in the taskbar.
- The agent's runs stay in the history like any other; `results.json` stores the approval they ran with.
- The threat model is ADR-004's: mistakes and over-initiative from the agent, not a hostile agent. For that, the sandbox (option 1) and, on targets you own, the server token (option 2) stay in the backlog.
