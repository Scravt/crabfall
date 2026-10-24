# ADR-004 — Approval and threat model

Status: **accepted** · 12 Oct 2026 · extended by [ADR-017](ADR-017-window-confirmation.md) (the agent asks for a run with `run --ask` and the person confirms it in a window)

## Context

Principle 1: Claude designs and analyzes, but only a person authorizes the load. Claude Code has a shell and can edit any file in the repo, so the guard can't depend on Claude "behaving" or on Claude Code's permissions. It has to live in the `crabfall` binary.

## What is protected and what isn't

**It protects against:**

- Claude running load on its own initiative or by mistake (`crabfall run` without approval);
- Claude changing an approved scenario while the approval still holds;
- Claude leaving the scenario untouched and redirecting the environment (for example, the `base_url` of `local` toward production);
- Claude approving itself (`crabfall approve` through a pipe, `yes | crabfall approve`).

**It doesn't protect against** an agent that acts deliberately maliciously with the user's permissions. That agent can generate load without `crabfall` (a `curl` loop), emulate a terminal or read the keyring. The guard prevents accidents and misuse of this tool; it doesn't replace trust in the agent or the security of the machine.

## Decision

### 1. What is signed

The approval is an HMAC-SHA256 signature over:

- the scenario's canonical hash (ADR-002, without `description` or `meta`);
- the **resolved environment**: base URL, `allowed_hosts`, variables (including those from `--var`) and `limits`;
- the format version.

The HMAC key is generated on the first `crabfall approve` and stored in the system keyring (Windows Credential Manager, Keychain on macOS, Secret Service on Linux), never in the repo.

Approvals are stored in `.crabfall/approvals/<scenario>.<environment>.json` and are not versioned, because the key is local to each machine.

### 2. `crabfall approve <scenario> --env <environment>`

- It requires a **real interactive terminal**: if stdin or stdout isn't a console, it fails.
- It shows the risk summary: target hosts, maximum request rate, VUs, duration, steps that write (`[write]`), and the diff against the last approval.
- It refuses if the scenario goes past the environment's `limits` or has pending `todo`s.
- It asks the person to **type the target host** ("y" is not enough).

### 3. `crabfall run <scenario> --env <environment>`

- It recomputes the hash of the scenario and of the resolved environment, and verifies the signature. If they don't match, it fails with a clear message and exits with its own code.
- It enforces the `limits` again at run time: if the engine gets close to going past them, it stops.
- Every host it connects to, redirects included, has to be in `allowed_hosts`.

### 4. `crabfall check` (smoke check, no approval)

The only thing Claude can run on its own. Rules:

- Every host has to resolve to loopback (`127.0.0.0/8` or `::1`); if any doesn't, it fails before sending anything.
- It doesn't follow redirects outside loopback.
- It runs `setup` and each flow **once**, serially, with 1 VU; it ignores `load`.
- It has a total cap on requests (50) and a per-request timeout.
- The output truncates response bodies (2 KB) and redacts secrets.

### 5. An extra defense in Claude Code

The integration (ADR-001) includes a `PreToolUse` hook that blocks `crabfall approve` and `crabfall run` when Claude invokes them. It is an extra layer against mistakes, not the main guard.

## Verification

Automated tests that have to fail (not enable `run`):

- editing the YAML after approving;
- changing `base_url`, `allowed_hosts`, `limits` or an environment variable;
- passing a `--var` different from the approved one;
- editing or copying an `approvals/` file by hand;
- running `approve` with redirected stdin (`echo host | crabfall approve ...`);
- `check` against a host that isn't loopback, or with a redirect going outside.

## Implementation (12 Oct 2026)

- **What is signed:** the canonical plan (`Plan::canonical`): URLs already resolved against `base_url`, method, headers (the default `user-agent` as `crabfall/<version>`), body, timeout and tags of each step; flow weights; the load resolved with its defaults; and from the environment, `base_url`, `allowed_hosts`, `vars` and `limits`. Also `setup`, `extract` and the resolved variables (scenario, environment and `--var`), but never the secrets' values. `description`, `meta`, `source`, `checks` and `thresholds` are left out: they don't change the traffic. Upgrading `crabfall` doesn't invalidate approvals: the default `user-agent` is signed without the version (24 Oct 2026; before, it carried it, and moving to 0.2.0 meant approving everything again). One set in the scenario is signed as is.
- **Signature:** HMAC-SHA256 over `crabfall-approval/<format>`, scenario, environment, the SHA-256 hash of the canonical plan, date and person. The key (32 random bytes) lives in the system keyring as `crabfall` / `approval-key`.
- **File:** `.crabfall/approvals/<scenario>.<environment>.json` also stores the approved plan. With it, `approve` shows the diff against the previous approval and `run` explains what changed when it refuses.
- **`crabfall approve`** requires an interactive terminal even if the target is loopback, and asks to type the first host that isn't loopback (or the only one, if they all are).
- **`crabfall run`** doesn't require a terminal: it verifies the signature. That way, later, an approval made by a person can be run from a script.
- **No expiry:** an approval holds until something changes. If needed, an expiry or a maximum number of runs can be added.
- **Tests:** `crates/crabfall-core/tests/approval.rs` (edits to the scenario and the environment, an edited file, one copied from another scenario or environment, another key, `--var` (added 24 Oct 2026), the default `user-agent`) and `crates/crabfall-cli/tests/guard.rs` (redirected stdin, no approval, a damaged file).

## Consequences

- Every load run goes through a visible human action, with the target typed in.
- Every change, however small, means approving again. That is intended.
- Approving from CI isn't covered; if needed, it will require a separate mechanism (for example, a key in a CI secret) and its own ADR.
