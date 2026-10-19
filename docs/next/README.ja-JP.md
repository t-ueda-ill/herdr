# Herdr（t-ueda-ill フォーク）

[English](README.md)

Herdrは、AIコーディングエージェント向けのターミナルワークスペースマネージャーです。
エージェントやシェルを別々のペインで動かし、状態を確認できます。
画面からデタッチしても、バックグラウンドサーバー上で処理を継続できます。

このリポジトリは、[herdrdev/herdr](https://github.com/herdrdev/herdr)から派生した[t-ueda-ill/herdr](https://github.com/t-ueda-ill/herdr)です。
Dockerコンテナ内でシェルを開く機能を追加し、このフォーク独自の安定版と更新を配布しています。

**変更の明示：** このREADMEは、上流版の構成と説明を置き換え、t-ueda-ill/herdr向けに書き直したものです。

## 主な機能

- プロジェクトをワークスペース、タブ、ターミナルペインで整理
- Claude CodeやCodexなど、既存のエージェントをそれぞれのターミナルで実行
- 作業中（working）、入力待ち（blocked）、待機中（idle）などの状態を確認
- サーバー上のプロセスを動かしたままデタッチし、再接続
- ローカルのターミナルとSSH接続先のマシンを利用
- ローカルやSSH接続先で稼働中のDockerコンテナ内にシェルを作成
- CLIとソケットAPIからワークスペースやペインを操作
- マウス、コンテキストメニュー、変更可能なキーボードショートカットで操作

デタッチ後もプロセスが動き続けるのは、サーバーが稼働している間です。
サーバーやマシンを再起動した場合は、保存済みのレイアウトを復元し、対応するエージェントのセッションを再開できますが、元のプロセスは終了しています。

## インストール

### LinuxとmacOS

```sh
curl -fsSL https://raw.githubusercontent.com/t-ueda-ill/herdr/master/distribution/install.sh | sh
```

x86_64とaarch64に対応しています。
既定のインストール先は`~/.local/bin`です。
このディレクトリを`PATH`に追加してください。

### Windows

PowerShellで実行します。

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://raw.githubusercontent.com/t-ueda-ill/herdr/master/distribution/install.ps1 | iex"
```

Windows x86_64版は、`herdr.exe`とアプリに同梱するConPTYランタイムを含むZIPで配布しています。
手動でインストールする場合も、ランタイムのファイルを実行ファイルと同じ場所に置いてください。

配布ファイルは[このフォークのリリース一覧](https://github.com/t-ueda-ill/herdr/releases)から取得できます。
このビルドを使う場合は、このフォークのインストーラーか配布ファイルを利用してください。
上流のインストール手順では、上流版が導入される場合があります。

## 基本操作

プロジェクトのディレクトリでHerdrを起動します。

```sh
cd /path/to/project
herdr
```

ペイン内でエージェントやシェルを起動してください。
クリックでペインを選び、境界をドラッグしてサイズを変更し、右クリックでメニューを開けます。

既定のプレフィックスキーは`Ctrl+B`です。
押して離してから、操作に対応するキーを押します。

| 操作 | プレフィックスの後に押すキー |
| --- | --- |
| 現在のショートカットを表示 | `?` |
| タブを作成 | `c` |
| 右／下に分割 | `v` / `-` |
| ペイン間を移動 | `h` / `j` / `k` / `l` |
| デタッチ | `q` |

再び`herdr`を実行すると、既定のセッションに再接続できます。

## Dockerコンテナ内のシェル

Herdrサーバーが動いている状態で、コンテナを一覧表示し、その中にシェルを開きます。

```sh
herdr container list
herdr workspace create --container myapp/web --label web
```

コンテナの指定には、Composeの`project/service`、コンテナ名、コンテナIDを使えます。
対象のコンテナは起動済みで、ホストからDocker CLIとデーモンにアクセスできる必要があります。

既存のワークスペースに追加する場合は、`herdr tab create`や`herdr pane split`に`--container`を指定します。
`--ssh HOST`を付けると、`~/.ssh/config`に設定した接続先のコンテナを一覧表示したり、その中にシェルを開いたりできます。
SSH接続には、鍵認証か接続済みの共有コネクションが必要です。
TUIの**open container**メニューからも開けます。

Windowsホストではコンテナコマンドを利用できません。
サーバー再起動後にコンテナのペインを復元すると、ホスト側のシェルが起動し、コンテナには自動で再接続しません。
オプションと制限は[コンテナのCLIリファレンス](website/src/content/docs/ja/cli-reference.mdx#コンテナ)を参照してください。

## 更新

Herdrセッションの外で実行します。

```sh
herdr update
```

このビルドはフォークのリリースマニフェストを確認し、ダウンロードしたファイルのチェックサムを検証します。
このフォークは安定版のみを配布し、プレビューチャンネルは提供していません。

## ドキュメント

- [クイックスタート](website/src/content/docs/ja/quick-start.mdx)
- [キーボード操作](website/src/content/docs/ja/keyboard.mdx)
- [設定](website/src/content/docs/ja/configuration.mdx)
- [コンテナコマンドを含むCLIリファレンス](website/src/content/docs/ja/cli-reference.mdx)
- [セッションの状態と復元](website/src/content/docs/ja/session-state.mdx)
- [ソケットAPI](website/src/content/docs/ja/socket-api.mdx)

これらのリンクは、このチェックアウトの作業用ドキュメントである`docs/next`を指します。
未リリースの変更を含む場合があるため、配布済みの変更は[リリースノート](https://github.com/t-ueda-ill/herdr/releases)で確認してください。
[上流のドキュメント](https://herdr.dev/docs/)も共通機能の参考になりますが、このフォークとは異なる場合があります。

## 開発

ビルドにはRustとZig 0.16.0が必要です。
リポジトリのビルドと検証には`just`を、テストには`cargo-nextest`とBunもインストールしてください。

```sh
git clone https://github.com/t-ueda-ill/herdr.git
cd herdr
just build
just test
just check
```

Windowsの`just build`は、同梱するConPTYランタイムもパッケージ化します。
Unixの`just check`にはWindows向けクロスコンパイルが含まれるため、[AGENTS.md](../../AGENTS.md#testing)のSDKセットアップが必要です。

変更を加える前に[AGENTS.md](../../AGENTS.md)を、IssueやPRを提出する前に[CONTRIBUTING.md](../../CONTRIBUTING.md)を読んでください。
フォークのリリース手順は[AGENTS.md](../../AGENTS.md#release-channels-t-ueda-illherdr-fork)に記載しています。

## ライセンスと派生元

Herdrは[Apache License, Version 2.0](../../LICENSE)で配布しています。
このフォークは[herdrdev/herdr](https://github.com/herdrdev/herdr)から派生したもので、上流プロジェクトと各貢献者は自身の著作物に関する権利を保持しています。
同梱する第三者コンポーネントには、それぞれのライセンスと通知が適用されます。
該当する文書は`vendor/`や`packaging/windows/licenses/`などに含まれています。
