# herdr

> これは [herdrdev/herdr](https://github.com/herdrdev/herdr) の fork [t-ueda-ill/herdr](https://github.com/t-ueda-ill/herdr) です。Docker コンテナ内でシェルを開く機能を追加し、独自にリリースを公開しています。`herdr update` はこの fork のリリースを追従します。

<p align="center">
  <img src="assets/logo.png" alt="herdr" width="100" />
</p>

<p align="center">
  <a href="https://herdr.dev">herdr.dev</a> · <a href="#インストール">インストール</a> · <a href="https://herdr.dev/ja/docs/quick-start/">クイックスタート</a> · <a href="https://herdr.dev/ja/docs/">ドキュメント</a>
</p>

<p align="center">
  <a href="README.md">English</a> · 日本語 · <a href="README.zh-CN.md">简体中文</a>
</p>

<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-666666?labelColor=333333" alt="Apache 2.0 license" /></a>
  <a href="https://github.com/t-ueda-ill/herdr/releases/latest"><img src="https://img.shields.io/github/v/release/t-ueda-ill/herdr?label=fork%20release&labelColor=333333&color=666666" alt="latest fork release" /></a>
  <a href="https://github.com/herdrdev/herdr/releases/latest"><img src="https://img.shields.io/github/v/release/herdrdev/herdr?label=upstream&labelColor=333333&color=666666" alt="latest upstream release" /></a>
</p>

---

https://github.com/user-attachments/assets/043ec09f-4bdd-41d5-aee0-8fda6b83e267

**コーディングエージェントを動かし続けるランタイム。**

- **作業を止めずにデタッチ**: クライアントを閉じても SSH が切れても、herdr はバックグラウンドのサーバーでターミナルを動かし続けます。サーバーやマシンを再起動した後は保存したレイアウトを復元し、対応エージェントのセッションを再開できます。元のプロセス自体は残りません。[セッション状態 →](https://herdr.dev/ja/docs/session-state/)
- **複数マシンを 1 つのウィンドウで**: ローカルの作業と保存済み SSH マシンをまとめて扱えます。エージェント一覧は統合され、再接続はマシンごとに独立しています。[リモートマシン →](https://herdr.dev/ja/docs/connecting-machines/)
- **詰まっているエージェントを探さなくてよい**: すべてのペインに working、blocked、idle のいずれかの印が付きます。エージェントが止まって回答を待っていれば、herdr がそれを知らせます。
- **エージェントネイティブ**: エージェントは CLI と socket API から herdr を操作できます。ペインを起動し、互いにプロンプトを送り、相手が本当にブロックされるまで待てます。[agent skill →](https://herdr.dev/ja/docs/agent-skill/)
- **起動中のコンテナ内でシェルを開く**: `herdr workspace create --container REF`、`herdr tab create --container REF`、`herdr pane split --container REF` は、起動中の Compose サービスやコンテナの中に `docker exec` で新しいシェルを開きます。ホストでも保存済み SSH マシン経由でも使えます。`herdr container list` で起動中のコンテナを確認できます。復元されたペインはコンテナ情報を保持します。この機能は fork 独自です。 [CLI リファレンス →](https://github.com/t-ueda-ill/herdr/blob/master/docs/next/website/src/content/docs/ja/cli-reference.mdx#コンテナ)
- **いつものツールをそのまま**: claude code、codex、cursor、opencode、grok などをそのまま使えます。herdr はそれらをラップも置き換えもせず、ターミナルを管理するだけです。エージェントを開発している場合は [herdr 対応を追加する →](https://herdr.dev/ja/docs/add-herdr-support/)
- **キーボードもマウスも同じように使える**: tmux 風のプレフィックスキーと、クリック、ドラッグ、分割の両方に対応します。ツールごとに決めるのではなく、その場その場で選べます。
- **プラグイン**: ペインとワークフローを拡張できます。[マーケットプレイス →](https://herdr.dev/ja/plugins/)
- **Rust 製の単一バイナリで Electron 不要**: 普段使っているターミナルでそのまま動きます。

---

## インストール

```bash
curl -fsSL https://raw.githubusercontent.com/t-ueda-ill/herdr/master/distribution/install.sh | sh
```

Windows: `powershell -ExecutionPolicy Bypass -c "irm https://raw.githubusercontent.com/t-ueda-ill/herdr/master/distribution/install.ps1 | iex"` · [エンドポイント保護された Windows](https://herdr.dev/ja/docs/windows-beta/) · [バイナリ](https://github.com/t-ueda-ill/herdr/releases)

`brew install herdr` と `mise use -g herdr` は upstream のビルドをインストールします。そのビルドは herdr.dev から更新されるため、この fork には追従しません。この fork は stable リリースのみを公開しており、`herdr channel set preview` は拒否されます。herdr セッションの外で `herdr update` を実行すると、fork の最新リリースをチェックサム検証のうえインストールします。

作業ディレクトリで起動します。

```bash
herdr
```

エージェントを動かし、ペインを分割したら、席を離れて構いません。`ctrl+b q` でデタッチ、`herdr` で再アタッチします。[クイックスタート →](https://herdr.dev/ja/docs/quick-start/)

## ドキュメント

upstream のドキュメントは [herdr.dev/docs](https://herdr.dev/ja/docs/) にあります。fork 独自機能は [`docs/next`](https://github.com/t-ueda-ill/herdr/tree/master/docs/next/website/src/content/docs) に記載しています。upstream へのリンク: [クイックスタート](https://herdr.dev/ja/docs/quick-start/) · [コンセプト](https://herdr.dev/ja/docs/concepts/) · [対応エージェント](https://herdr.dev/ja/docs/agents/) · [キーボード](https://herdr.dev/ja/docs/keyboard/) · [設定](https://herdr.dev/ja/docs/configuration/) · [セッション状態](https://herdr.dev/ja/docs/session-state/) · [マシン接続](https://herdr.dev/ja/docs/connecting-machines/) · [リモート](https://herdr.dev/ja/docs/persistence-remote/) · [インテグレーション](https://herdr.dev/ja/docs/integrations/) · [エージェントに herdr 対応を追加する](https://herdr.dev/ja/docs/add-herdr-support/) · [プラグイン](https://herdr.dev/ja/docs/plugins/) · [socket API](https://herdr.dev/ja/docs/socket-api/)

## 謝辞

これまでのスポンサーと支援者は [SPONSORS.md](./SPONSORS.md) に掲載しています。ありがとうございます 🐑

upstream のエンタープライズ / パートナーシップ窓口: hey@herdr.dev

## エージェントへの指示

このリポジトリで作業する AI エージェントは、変更を加える前に [`AGENTS.md`](./AGENTS.md) を、issue や PR を作成する前に [`CONTRIBUTING.md`](./CONTRIBUTING.md) を読んでください。

## 開発

```bash
git clone https://github.com/t-ueda-ill/herdr
cd herdr
cargo build --release

just test        # ユニットテスト
just check       # フォーマット、テスト、保守チェック
```

リリース手順: `Cargo.toml` と `Cargo.lock` を次の fork バージョンに上げます(upstream の `0.9.5` に対して fork は `0.9.501`、`0.9.502` と続きます)。`master` を push してから `v<version>` タグを push します。`.github/workflows/release-fork.yml` が 5 つのバイナリをビルドして GitHub Release を公開し、`herdr update` が参照する `distribution/latest.json` をコミットします。詳細は [`AGENTS.md`](./AGENTS.md) のリリース節を参照してください。

## ライセンス

Herdr は [Apache License 2.0](LICENSE) の下で提供されます。
