# Herdr (t-ueda-ill fork)

[日本語](README.ja-JP.md)

Herdr is a terminal workspace manager for AI coding agents. Run agents and shells
in separate panes, monitor their status, and detach while they continue working
in a background server.

This repository is [t-ueda-ill/herdr](https://github.com/t-ueda-ill/herdr), a fork
of [herdrdev/herdr](https://github.com/herdrdev/herdr). It adds Docker container
shells and distributes its own stable releases and updates.

**Modification notice:** This README has been rewritten for the t-ueda-ill/herdr
fork, replacing the upstream presentation.

## Features

- Organize projects into workspaces, tabs, and terminal panes.
- Run existing coding agents, including Claude Code and Codex, in their own terminals.
- Track agent states such as working, blocked, and idle.
- Detach and reconnect without stopping the server's running processes.
- Work with local terminals and remote machines over SSH.
- Open shells in running Docker containers, locally or over SSH.
- Control workspaces and panes through the CLI and socket API.
- Use mouse controls, context menus, or configurable keyboard shortcuts.

Detaching keeps processes alive while the server is running. After a server or
machine restart, saved layouts can be restored and supported agent sessions can
be resumed; the original processes do not survive the restart.

## Install

### Linux and macOS

```sh
curl -fsSL https://raw.githubusercontent.com/t-ueda-ill/herdr/master/distribution/install.sh | sh
```

The installer supports x86_64 and aarch64 and installs to `~/.local/bin` by
default. Make sure that directory is on your `PATH`.

### Windows

Run in PowerShell:

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://raw.githubusercontent.com/t-ueda-ill/herdr/master/distribution/install.ps1 | iex"
```

The Windows x86_64 release is a ZIP containing `herdr.exe` and its app-local
ConPTY runtime. Keep the runtime files alongside the executable when installing
manually.

All release assets are available on the [fork's releases page](https://github.com/t-ueda-ill/herdr/releases).
Use this fork's installer or release assets to install this build; upstream
installation instructions may select the upstream distribution.

## Start using Herdr

Start Herdr from your project directory:

```sh
cd /path/to/project
herdr
```

Launch your agent or shell in a pane. Click to focus panes, drag split borders
to resize, and right-click to open context menus.

The default keyboard prefix is `Ctrl+B`: press it, release it, then press the
action key.

| Action | Keys after the prefix |
| --- | --- |
| Show current shortcuts | `?` |
| Create a tab | `c` |
| Split right / down | `v` / `-` |
| Move between panes | `h` / `j` / `k` / `l` |
| Detach | `q` |

Run `herdr` again to reattach to the default session.

## Docker container shells

With a Herdr server running, list containers and open a shell in one:

```sh
herdr container list
herdr workspace create --container myapp/web --label web
```

The container reference can be a Compose `project/service`, a container name,
or a container ID. The container must already be running, and the host needs
access to the Docker CLI and daemon.

Use `--container` with `herdr tab create` or `herdr pane split` to add a
container shell to an existing workspace. Add `--ssh HOST` to list or open
containers on a host configured in `~/.ssh/config`; this requires key-based SSH
access or an existing shared connection. The TUI also offers an **open
container** menu.

Container commands are unsupported on Windows hosts. After a server restart,
restoring a container pane starts a host shell; it does not reconnect to the
container automatically. See the [container CLI reference](docs/next/website/src/content/docs/cli-reference.mdx#containers)
for options and limitations.

## Updates

Run outside a Herdr session:

```sh
herdr update
```

This build checks the fork's release manifest and verifies the downloaded
asset's checksum. The fork publishes stable releases only; the preview channel
is unavailable.

## Documentation

- [Quick start](docs/next/website/src/content/docs/quick-start.mdx)
- [Keyboard controls](docs/next/website/src/content/docs/keyboard.mdx)
- [Configuration](docs/next/website/src/content/docs/configuration.mdx)
- [CLI reference, including container commands](docs/next/website/src/content/docs/cli-reference.mdx)
- [Session state and restoration](docs/next/website/src/content/docs/session-state.mdx)
- [Socket API](docs/next/website/src/content/docs/socket-api.mdx)

These links point to the working documentation for this checkout under
`docs/next`; it can include changes that have not been released. See the
[release notes](https://github.com/t-ueda-ill/herdr/releases) for shipped changes.
The [upstream documentation](https://herdr.dev/docs/) covers shared features,
but can differ from this fork.

## Development

Building requires Rust and Zig 0.16.0. Install `just` to use the repository's
build and validation recipes, and `cargo-nextest` and Bun for the test recipes.

```sh
git clone https://github.com/t-ueda-ill/herdr.git
cd herdr
just build
just test
just check
```

On Windows, `just build` also packages the app-local ConPTY runtime. On Unix,
`just check` includes Windows cross-compilation, which requires the SDK setup
described in [AGENTS.md](AGENTS.md#testing).

Read [AGENTS.md](AGENTS.md) before changing the repository and
[CONTRIBUTING.md](CONTRIBUTING.md) before submitting issues or pull requests.
Fork release procedures are documented in
[AGENTS.md](AGENTS.md#release-channels-t-ueda-illherdr-fork).

## License and origin

Herdr is distributed under the [Apache License, Version 2.0](LICENSE).
This fork is derived from [herdrdev/herdr](https://github.com/herdrdev/herdr);
the upstream project and its contributors retain their rights in their work.
Bundled third-party components retain their respective licenses and notices,
including those under `vendor/` and `packaging/windows/licenses/`.
