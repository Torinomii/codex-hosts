# codex-hosts

<p align="center">
  <a href="https://github.com/Torinomii/codex-hosts/actions/workflows/release.yml"><img src="https://github.com/Torinomii/codex-hosts/actions/workflows/release.yml/badge.svg" alt="Release ビルド" /></a>
  <a href="../../LICENSE"><img src="https://img.shields.io/badge/License-Apache--2.0-D22128.svg" alt="Apache 2.0 ライセンス" /></a>
  <img src="https://img.shields.io/badge/Windows-10%2B-0078D4?logo=windows&logoColor=white" alt="Windows 10 以降" />
  <img src="https://img.shields.io/badge/Rust-1.92%2B-000000?logo=rust&logoColor=white" alt="Rust 1.92 以降" />
  <a href="https://linux.do/"><img src="https://img.shields.io/badge/LINUX-DO-FFB003" alt="LINUX DO コミュニティ" /></a>
</p>

[English](../../README.md) | [简体中文](README_zh-CN.md) | [繁體中文](README_zh-TW.md) | [日本語](README_ja.md)

`codex-hosts` は Codex 向けの Windows SSH / Telnet ホスト管理ツールです。Codex がパスワードや鍵に触れることなく、安全に接続を開始できます。

![Codex Hosts メインウィンドウ](../../Main.png)

## 主な機能

- エイリアス、アドレス、ポート、ユーザー名、プロトコル、認証方式、任意の秘密鍵パス、信頼済み SSH ホスト鍵、任意の踏み台ホストを保存します。
- 認証情報を含まない JSON テンプレートをダウンロードして複数のホストをインポートでき、既存のエイリアスや認証情報を上書きしません。
- 保存済みホストを一括テストし、ホストごとに 10 秒で打ち切り、成功と失敗を色で明確に表示します。
- 複数のホストを選択して一括削除でき、ほかのホストが使用中の踏み台ホストは保護されます。
- SSH のパスワード認証とキーボードインタラクティブ認証、および任意のパスフレーズ付き OpenSSH 秘密鍵に対応します。
- パスワードと秘密鍵のパスフレーズは Windows 資格情報マネージャーだけに保存します。
- 保存済みかつ検証済みの SSH ホストから多段の踏み台チェーンを構成します。
- Codex はエイリアスを指定した下書きを事前作成し、会話で資格情報を求めることなく、保存、信頼、またはキャンセルの結果を待機できます。

## インストール

### Release をダウンロード

ビルド済みパッケージは 64 ビット版 Windows 10 以降に対応し、[Releases](https://github.com/Torinomii/codex-hosts/releases/latest) からダウンロードできます。

### インストール

`bin\codex-hosts.exe` を `skill\codex-hosts\bin\codex-hosts.exe` にコピーし、`skill\codex-hosts` ディレクトリを `%USERPROFILE%\.codex\skills\codex-hosts` にコピーします。

### Codex プロンプトでインストール

```text
https://github.com/Torinomii/codex-hosts/releases/latest から codex-hosts の最新版をダウンロードしてインストールしてください。現在の環境の Skill インストールディレクトリを自動的に特定し、完全な codex-hosts Skill と実行ファイルを正しい場所にインストールして、Skill、実行ファイル、および必要な設定ファイルが存在することを確認してください。
```

### ソースからインストール

ソースからのビルドには Rust 1.92 以降と MSVC ツールチェーンが必要です。

```
git clone https://github.com/Torinomii/codex-hosts.git
cd codex-hosts
cargo build --locked --release
```

`target\release\codex-hosts.exe` を `skill\codex-hosts\bin\codex-hosts.exe` にコピーし、完全な `skill\codex-hosts` ディレクトリを `%USERPROFILE%\.codex\skills\codex-hosts` としてインストールします。

### 使用方法

1. skill と実行ファイルを正しくインストールすると、Codex が新しいリモート接続を必要としたときに codex-hosts を開き、パスワードの入力を求めます。
2. あらかじめ codex-hosts に接続情報を入力し、保存済みの情報を Codex に認識させて接続することもできます。

## Codex との連携

リポジトリの `skill\codex-hosts` にはポータブルな Codex skill が含まれています。インストール後、`SKILL.md` はインストール先の skill ディレクトリを基準に `bin\codex-hosts.exe` を参照するため、リポジトリ固有の絶対パスを変更する必要はありません。

GUI 編集モードには、秘密情報を含まない初期値を渡せます。

```
.\bin\codex-hosts.exe --codex-edit `
  --alias example `
  --host server.example.com `
  --port 22 `
  --user operator `
  --protocol ssh `
  --auth password `
  --result-file result.json
```

パスワードや秘密鍵のパスフレーズを、会話、コマンドライン引数、JSON、スクリプト、ログに含めないでください。ユーザーはマスクされた GUI 入力欄だけで資格情報を入力します。

ツールモードでは、同じ実行ファイルが資格情報を含まない要求ファイルを読み、結果ファイルを書き込みます。

```json
{"action":"list_hosts"}
{"action":"probe","alias":"example"}
{"action":"exec","alias":"example","command":"hostname"}
```

`probe` と `exec` には、操作ごとの `connect_timeout_ms` と `command_timeout_ms` を指定できます。アプリケーション側のタイムアウトが不要な場合は省略します。保存済みホストには固定タイムアウトを保持せず、リモートコマンドはリモート OS を推測せず、そのまま送信されます。

## セキュリティ境界

- パスワードと秘密鍵のパスフレーズは Windows 資格情報マネージャーだけに保存し、Codex には返しません。
- SSH フィンガープリントは明示的に固定し、自動置換しません。
- 検証済み SSH プロファイルだけを踏み台として利用できます。チェーンの循環を検査し、最大 8 ホストに制限します。
- Telnet は平文プロトコルです。このリスクを明示的に受け入れた信頼できるネットワーク内でのみ使用してください。
- コマンド出力の取得量は出力ストリームごとに 1 MiB までです。
