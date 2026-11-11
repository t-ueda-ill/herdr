# herdr (t-ueda-ill/herdr fork)

Terminal based agent runtime for coding agents.

`AGENTS.md` is the full rule set and stays aligned with upstream. This file is
the always-loaded part for Claude Code. Rules here are taken from `AGENTS.md`
and adapted to the fork only under "Fork workflow"; when the two differ
anywhere else, `AGENTS.md` wins and this file needs a fix.

## Read the matching `AGENTS.md` section before these tasks

| Before you... | Read `AGENTS.md` section |
| --- | --- |
| change `src/detect/` or `distribution/agent-detection/` | Agent Detection Updates, and the screen-detection bullet under Principles |
| touch `vendor/` | Vendored libghostty-vt |
| edit anything under `docs/`, `README.md`, `CHANGELOG.md`, `skills/herdr/SKILL.md`, or `distribution/latest.json` | Docs |
| bump the version, tag, or release | Release Channels |
| add work to view computation, rendering, background-pane resizing, PTY parsing, detection, or client frame fanout | Multiplicative performance paths |
| change the client endpoint, a codec, handshake or snapshot fields, or an advertised API method | Stable client endpoint contract |
| start a broad refactor, or set up Windows cross-compilation | Testing |
| push, open an issue, or open a PR against `herdrdev/herdr` | Scope and Audience, External contributor guardrail |

The project-local skills live in `.agents/skills/` (`herdr-throwaway-repro`,
`herdr-pre-release-audit`, `triage`). Read the skill's `SKILL.md` when
`AGENTS.md` names it.

## Fork workflow

This checkout is the fork `t-ueda-ill/herdr`. `origin` is the fork and
`upstream` is `herdrdev/herdr`.

- Do not push branches, open issues, or open PRs against `herdrdev/herdr`. The
  External contributor guardrail in `AGENTS.md` governs anything sent there.
- Work lands in the fork through pull requests. Never merge a pull request; the
  fork owner merges.
- Before committing, propose the commit message and get alignment.
- Small changes are fine in the main checkout. Use a task worktree at
  `../herdr-worktrees/<task-slug>` for bigger features, or when unrelated
  changes are already in progress in the main checkout. Do not create nested
  worktrees.
- After opening or updating a pull request, watch all checks to completion and
  wait for the review bot on the latest pushed commit. Fix findings you agree
  with and reply with the fix; reply inline with a concise technical reason
  when you disagree.
- This fork publishes stable releases only. Skip preview-channel steps in
  `AGENTS.md`.

## Principles

- **State is separated from runtime.** `AppState` is pure data, testable without PTYs or async. `PaneState` is separate from `PaneRuntime`. Workspace logic doesn't need real terminals.
- **Render is pure.** `compute_view()` handles geometry and mutations. `render()` takes `&AppState` and only draws. Never mutate state during render.
- **No god objects.** If a module is doing too many things, split it. `app/` is already split into state, actions, and input. Keep it that way.
- **Platform code is isolated.** OS-specific behavior lives in the matching `src/platform/<os>.rs` file, with only shared traits, types, wrappers, and testable contracts in `src/platform/mod.rs`. Core modules don't have `#[cfg(target_os)]`.
- **Detection is decoupled.** The detector reads a screen snapshot, never touches the parser or viewport state.
- **UI patterns should be reused.** Herdr is a mouse-first TUI. New dialogs, onboarding, settings, and post-update flows should follow the existing UI/UX language and interaction patterns instead of inventing one-off screens.

### Runtime/client boundary guardrail

Herdr is migrating toward a server-owned runtime protocol with the TUI as one client. New work should not deepen the current server/TUI coupling.

Before adding state, API fields, events, commands, or socket messages, classify the feature:

- Shared runtime/session fact: belongs in server state and should be exposed through the JSON API/event path when practical.
- TUI presentation state: belongs only in the TUI/client layer.

Do not add new shared behavior that only works through the private TUI client socket. Use neutral server/API names, not UI-surface names like sidebar, row, card, or widget.

### Compatibility contracts

- Named core codecs are immutable. Do not add, remove, reorder, or reinterpret fields or enum variants reachable from a published codec. Introduce a new codec name and keep the old codec as a fallback instead.
- Frozen endpoint fixtures, bincode digests, wire-tag tests, and `tests/fixtures/endpoint-method-shapes-v1.json` are compatibility contracts. Never update a generation-1 expectation merely to bless a wire change; create and negotiate a new codec or method.

## Testing

Use `just` recipes by default instead of invoking cargo or scripts directly.

```bash
just test               # cargo nextest + maintenance script tests
just check              # formatting check + cargo nextest + maintenance script tests
```

Run `just check` before committing unless the fork owner explicitly accepts narrower validation. Do not bypass failing checks; fix the failure or explain exactly why a narrower check is enough.

New `AppState` or `Workspace` behavior should be testable with `AppState::test_new()` and `Workspace::test_new()` without PTYs.

When testing a new Herdr build from inside an existing Herdr session, use
`cargo run -- ...` and clear inherited Herdr socket overrides so the debug
binary talks to the debug `herdr-dev` server instead of the installed stable
server:

```bash
env -u HERDR_SOCKET_PATH -u HERDR_CLIENT_SOCKET_PATH cargo run -- <command>
```

## Files with standing prohibitions

- `docs/preview/website/`: never edit it manually.
- `docs/next/CHANGELOG.md`: normal feature and fix work must not edit it.
- Root `README.md`, root `CHANGELOG.md`, `docs/versions/`, `distribution/latest.json`, `skills/herdr/SKILL.md`: do not edit in normal feature or fix work unless it is a focused correction to already-published documentation or explicitly requested.
- `~/.config/herdr/agent-detection/<agent>.toml`: never overwrite or remove a pre-existing override without alignment.
- Put local PRDs, planning notes, and exploratory specs under `.local/prd/`.

## Commit Style

Use lowercase conventional commits, no emojis, and no AI co-author lines. Commit subjects feed release notes, so keep them descriptive.

When a normal feature or fix commit relates to a GitHub issue, add a commit body line `refs #<issue-number>` after the subject. Do not use GitHub closing keywords like `fixes #<issue-number>`, `closes #<issue-number>`, or `resolves #<issue-number>` in normal commits; release CI closes referenced issues after the GitHub Release is created.

## Code Conventions

- Rust: no `unwrap()` in production code. Use `tracing` for logging. Use `#[allow]` only with a comment explaining why.
- Rust platform-specific code must be compile-gated. Put OS APIs and substantial OS behavior in `src/platform/`; when platform checks are needed elsewhere, use `#[cfg(windows)]`, `#[cfg(unix)]`, or target-specific `#[cfg(...)]` on imports, fields, functions, impls, and match arms so Windows-only code does not compile into Unix builds and Unix-only code does not compile into Windows builds. Use `cfg!(...)` only for pure cross-platform policy constants whose branches both compile on every target.
- Don't add dependencies without a reason. Check whether existing dependencies cover the need first.
- Integration asset versions (`HERDR_INTEGRATION_VERSION` markers and matching `*_INTEGRATION_VERSION` constants) are migration versions relative to the latest released tag, not per-commit counters on `master`. If an integration asset changes multiple times between releases, bump it once from the version in the latest release.
- When changing the server/client wire protocol, compare `src/protocol/wire.rs::PROTOCOL_VERSION` against published protocols. Bump it when the current source protocol has already been published and the wire format changes incompatibly. Do not bump it again for multiple incompatible changes before that protocol is published. Update hardcoded protocol expectations and manual protocol fixtures in tests.
