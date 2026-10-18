# herdr

> this is the [t-ueda-ill/herdr](https://github.com/t-ueda-ill/herdr) fork of [herdrdev/herdr](https://github.com/herdrdev/herdr). it adds shells inside docker containers and publishes its own releases; `herdr update` follows this fork.

<p align="center">
  <img src="assets/logo.png" alt="herdr" width="100" />
</p>

<p align="center">
  <a href="https://herdr.dev">herdr.dev</a> · <a href="#install">install</a> · <a href="https://herdr.dev/docs/quick-start/">quick start</a> · <a href="https://herdr.dev/docs/">docs</a>
</p>

<p align="center">
  English · <a href="README.ja-JP.md">日本語</a> · <a href="README.zh-CN.md">简体中文</a>
</p>

<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-666666?labelColor=333333" alt="Apache 2.0 license" /></a>
  <a href="https://github.com/t-ueda-ill/herdr/releases/latest"><img src="https://img.shields.io/github/v/release/t-ueda-ill/herdr?label=fork%20release&labelColor=333333&color=666666" alt="latest fork release" /></a>
  <a href="https://github.com/herdrdev/herdr/releases/latest"><img src="https://img.shields.io/github/v/release/herdrdev/herdr?label=upstream&labelColor=333333&color=666666" alt="latest upstream release" /></a>
</p>

---

https://github.com/user-attachments/assets/043ec09f-4bdd-41d5-aee0-8fda6b83e267

**the runtime your coding agents live on.**

- **detach without stopping work** — herdr keeps terminals running in a background server when you close the client or lose your SSH connection. after a server or machine restart, herdr restores the saved layout and can resume supported agent sessions; the original processes do not survive. [session state →](https://herdr.dev/docs/session-state/)
- **several machines, one window** — keep local work and saved ssh machines together, with a combined agent list and independent reconnects. [remote machines →](https://herdr.dev/docs/connecting-machines/)
- **never hunt for the stuck one** — every pane is marked working, blocked, or idle. when an agent stops and needs an answer, herdr says so.
- **agent-native** — agents drive herdr through the cli and socket api: they can spawn panes, prompt each other, and wait until another agent is genuinely blocked. [agent skill →](https://herdr.dev/docs/agent-skill/)
- **shells inside running containers** — `herdr workspace create --container REF`, `herdr tab create --container REF`, and `herdr pane split --container REF` open the new shell with `docker exec` inside a running compose service or container, on the host or over a saved ssh machine. `herdr container list` shows what is running. restored panes keep their container. *(fork feature)* [cli reference →](https://github.com/t-ueda-ill/herdr/blob/master/docs/next/website/src/content/docs/cli-reference.mdx#containers)
- **runs what you already run** — claude code, codex, cursor, opencode, grok and the rest. herdr doesn't wrap or replace them; it owns their terminals. building an agent? [add herdr support →](https://herdr.dev/docs/add-herdr-support/)
- **keyboard and mouse, both first-class** — tmux-style prefix keys *and* click, drag, split. pick per moment, not per tool.
- **plugins** — extend panes and workflows. [browse the marketplace →](https://herdr.dev/plugins/)
- **one rust binary, no electron** — runs in whatever terminal you already use.

---

## install

```bash
curl -fsSL https://raw.githubusercontent.com/t-ueda-ill/herdr/master/distribution/install.sh | sh
```

windows: `powershell -ExecutionPolicy Bypass -c "irm https://raw.githubusercontent.com/t-ueda-ill/herdr/master/distribution/install.ps1 | iex"` · [endpoint-protected Windows](https://herdr.dev/docs/windows-beta/) · [binaries](https://github.com/t-ueda-ill/herdr/releases)

`brew install herdr` and `mise use -g herdr` install the upstream build, which updates from herdr.dev, not from this fork. this fork publishes stable releases only; `herdr channel set preview` is rejected. `herdr update` (run outside a herdr session) installs the latest fork release and verifies its checksum.

then start it where the work lives:

```bash
herdr
```

run your agents, split panes, walk away. `ctrl+b q` detaches, `herdr` reattaches. [quick start →](https://herdr.dev/docs/quick-start/)

## docs

upstream docs live at [herdr.dev/docs](https://herdr.dev/docs/); fork-only features are documented in [`docs/next`](https://github.com/t-ueda-ill/herdr/tree/master/docs/next/website/src/content/docs). upstream links: [quick start](https://herdr.dev/docs/quick-start/) · [concepts](https://herdr.dev/docs/concepts/) · [supported agents](https://herdr.dev/docs/agents/) · [keyboard](https://herdr.dev/docs/keyboard/) · [configuration](https://herdr.dev/docs/configuration/) · [session state](https://herdr.dev/docs/session-state/) · [connecting machines](https://herdr.dev/docs/connecting-machines/) · [remote](https://herdr.dev/docs/persistence-remote/) · [integrations](https://herdr.dev/docs/integrations/) · [add herdr support to your agent](https://herdr.dev/docs/add-herdr-support/) · [plugins](https://herdr.dev/docs/plugins/) · [socket api](https://herdr.dev/docs/socket-api/)

## thanks

every past sponsor and backer is listed in [SPONSORS.md](./SPONSORS.md) — thank you 🐑

upstream enterprise / partnership: hey@herdr.dev

## agent instructions

if you are an ai agent helping with this repository, read [`AGENTS.md`](./AGENTS.md) before making changes and read [`CONTRIBUTING.md`](./CONTRIBUTING.md) before opening issues or PRs.

## development

```bash
git clone https://github.com/t-ueda-ill/herdr
cd herdr
cargo build --release

just test        # unit tests
just check       # formatting, tests, and maintenance checks
```

releases: bump `Cargo.toml`/`Cargo.lock` to the next fork version (upstream `0.9.5` → fork `0.9.501`, `0.9.502`, …), push `master`, then push the tag `v<version>`. `.github/workflows/release-fork.yml` builds the five binaries, publishes the GitHub release, and commits `distribution/latest.json` so `herdr update` sees it. see the release section of [`AGENTS.md`](./AGENTS.md).

## license

Herdr is licensed under the [Apache License 2.0](LICENSE).
