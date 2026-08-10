# codex-hosts

<p align="center">
  <a href="https://github.com/Torinomii/codex-hosts/actions/workflows/release.yml"><img src="https://github.com/Torinomii/codex-hosts/actions/workflows/release.yml/badge.svg" alt="Release ビルド" /></a>
  <a href="../../LICENSE"><img src="https://img.shields.io/badge/License-Apache--2.0-D22128.svg" alt="Apache 2.0 ライセンス" /></a>
  <img src="https://img.shields.io/badge/Windows-10%2B-0078D4?logo=windows&logoColor=white" alt="Windows 10 以降" />
  <img src="https://img.shields.io/badge/Rust-1.92%2B-000000?logo=rust&logoColor=white" alt="Rust 1.92 以降" />
  <a href="https://linux.do/"><img src="https://img.shields.io/badge/LINUX-DO-FFB003" alt="LINUX DO コミュニティ" /></a>
</p>

[English](../../README.md) | [简体中文](README_zh-CN.md) | [繁體中文](README_zh-TW.md) | [日本語](README_ja.md)

`codex-hosts` は Codex 向けの Windows SSH / Telnet ホスト管理ツールです。Codex にパスワードや鍵を触れさせず、安全に接続できます。

![Codex Hosts メインウィンドウ](../../Main.png)

## 主な機能

- Codex が接続に使うサーバー情報を保存します。
- パスワード、通常の OpenSSH 鍵、FIDO/YubiKey などのハードウェア鍵に対応します。
- Windows OpenSSH Agent または Pageant に読み込まれている ID に対応します。

## インストール

### 直接ダウンロード

ビルド済みバージョンは 64 ビット版 Windows 10 以降に対応しています。[Releases](https://github.com/Torinomii/codex-hosts/releases/latest) からダウンロードできます。

手動でインストールする場合：

1. `bin\codex-hosts.exe` を `skill\codex-hosts\bin\codex-hosts.exe` に配置します。
2. 完全な `skill\codex-hosts` フォルダーを `%USERPROFILE%\.codex\skills\codex-hosts` にコピーします。

Codex にインストールを依頼することもできます：

```text
https://github.com/Torinomii/codex-hosts/releases/latest から最新版の codex-hosts をダウンロードしてインストールしてください。現在の環境の Skill インストールディレクトリを自動的に特定し、完全な Skill と実行ファイルをインストールして、必要なファイルがすべて配置されていることを確認してください。
```

### ソースからビルド

Rust 1.92 以降と MSVC ツールチェーンが必要です：

```powershell
git clone https://github.com/Torinomii/codex-hosts.git
cd codex-hosts
cargo build --locked --release
```

ビルドが完了したら、`target\release\codex-hosts.exe` を `skill\codex-hosts\bin\codex-hosts.exe` にコピーし、完全な Skill フォルダーをインストールします。

## クイックスタート

1. `codex-hosts.exe` を開き、ホストを新規作成します。
2. エイリアス、アドレス、ポート、ユーザー名を入力し、ログイン方法を選んで保存します。

<details>
<summary>インターフェース</summary>

### Codex またはスクリプトから呼び出す

GUI 編集モードには、秘密情報を含まない接続情報を初期値として渡せます：

```powershell
.\bin\codex-hosts.exe --codex-edit `
  --alias example `
  --host server.example.com `
  --port 22 `
  --user operator `
  --protocol ssh `
  --auth password `
  --result-file result.json
```

認証引数には固定の名前を使います：

- `password`：パスワード認証。
- `private-key` / `private_key`：鍵ファイルまたは FIDO ハンドル。`fido-handle` も使用できます。
- `ssh-agent` / `ssh_agent`：実行中の SSH Agent または Pageant。

ツールモードは要求ファイルを読み、結果ファイルを書き出します。どちらのファイルにも資格情報を含めてはいけません：

```json
{"action":"capabilities"}
{"action":"list_hosts"}
{"action":"agent_identities"}
{"action":"fido_identities"}
{"action":"probe","alias":"example"}
{"action":"exec","alias":"example","command":"hostname"}
{"action":"exec_many","alias":"example","commands":["hostname","uptime"],"max_concurrency":8}
{"action":"batch_probe","aliases":["web-1","web-2"],"max_concurrency":8,"batch_timeout_ms":30000}
{"action":"batch_exec","aliases":["web-1","web-2"],"command":"uptime","max_concurrency":8,"batch_timeout_ms":30000}
```

`exec_many` は一度だけ認証し、同じ SSH 接続上の複数チャネルで短いコマンドを並行実行します。ハードウェアキーでは、通常、一組のコマンドを 1 回の認証にまとめられます。`agent_identities` と `fido_identities` が返すのは公開されている ID 情報と公開鍵だけです。リモートコマンドを実行するときは、結果の `output_truncated` を確認し、サイズ制限により出力が省略されていないか確かめてください。

</details>

## セキュリティ境界

- パスワードと鍵ファイルのパスフレーズは Windows 資格情報マネージャーにのみ保存します。FIDO PIN はその操作中だけ使用し、保存しません。
- SSH ホストのフィンガープリントはユーザーが明示的に確認する必要があります。保存済みのフィンガープリントを自動で置き換えることはありません。
- FIDO の直接署名で Agent サービスを起動または有効化することはなく、Agent 転送も常に無効です。
- 検証済み SSH ホストだけを踏み台として使えます。チェーンは最大 8 ホストで、循環も検査します。
- Telnet はアカウント情報とデータを平文で送信します。リスクを明示的に受け入れられる信頼済みネットワークでのみ使用してください。
- 1 コマンドの出力は最大 1 MiB です。`exec_many` または一括処理の完成した JSON 結果は最大 8 MiB です。出力の受信中に予算を適用し、大きな JSON のコピーを複数メモリ上に作りません。
