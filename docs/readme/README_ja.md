# codex-hosts

[English](../../README.md) | [简体中文](README_zh-CN.md) | [繁體中文](README_zh-TW.md) | [日本語](README_ja.md)

`codex-hosts` は Codex 向けの Windows SSH / Telnet ホスト管理ツールです。Codex が平文パスワードに触れたり入力したりすることなく、安全に接続を開始できます。

![Codex Hosts メインウィンドウ](../../Main.png)

## 主な機能

- エイリアス、アドレス、ポート、ユーザー名、プロトコル、認証方式、任意の秘密鍵パス、信頼済み SSH ホスト鍵、任意の踏み台ホストを保存します。
- SSH のパスワード認証とキーボードインタラクティブ認証、および任意のパスフレーズ付き OpenSSH 秘密鍵に対応します。
- パスワードと秘密鍵のパスフレーズは Windows 資格情報マネージャーだけに保存します。
- 保存済みかつ検証済みの SSH ホストから多段の踏み台チェーンを構成します。
- リスクを明示的に受け入れたレガシー環境では Telnet を利用できます。Telnet の資格情報と通信は暗号化されません。
- Codex はエイリアスを指定した下書きを事前作成し、会話で資格情報を求めることなく、保存、信頼、またはキャンセルの結果を待機できます。

## インストール

### Release をダウンロード

ビルド済みパッケージは 64 ビット版 Windows 10 以降に対応します。[Releases](https://github.com/Torinomii/codex-hosts/releases/latest)。手動でワークフローを開始した場合は、

### ソースからインストール

ソースからのビルドには Rust 1.92 以降と MSVC ツールチェーンが必要です。

```
git clone https://github.com/Torinomii/codex-hosts.git
cd codex-hosts
cargo build --locked --release
```

`target\release\codex-hosts.exe` を `skill\codex-hosts\bin\codex-hosts.exe` にコピーし、完全な `skill\codex-hosts` ディレクトリを `%USERPROFILE%\.codex\skills\codex-hosts` としてインストールします。

### Codex 用の汎用インストールプロンプト

```text
codex-hosts をインストールしてください。現在の環境の Skill インストールディレクトリを自動的に特定し、完全な codex-hosts Skill を正しい場所にインストールして、実行ファイルと必要な設定ファイルが存在することを確認してください。
```

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
