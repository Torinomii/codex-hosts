# codex-hosts

<p align="center">
  <a href="https://github.com/Torinomii/codex-hosts/actions/workflows/release.yml"><img src="https://github.com/Torinomii/codex-hosts/actions/workflows/release.yml/badge.svg" alt="Release ビルド" /></a>
  <a href="../../LICENSE"><img src="https://img.shields.io/badge/License-Apache--2.0-D22128.svg" alt="Apache 2.0 ライセンス" /></a>
  <img src="https://img.shields.io/badge/Windows-10%2B-0078D4?logo=windows&logoColor=white" alt="Windows 10 以降" />
  <img src="https://img.shields.io/badge/Rust-1.92%2B-000000?logo=rust&logoColor=white" alt="Rust 1.92 以降" />
  <a href="https://linux.do/"><img src="https://img.shields.io/badge/LINUX-DO-FFB003" alt="LINUX DO コミュニティ" /></a>
</p>

[English](../../README.md) | [简体中文](README_zh-CN.md) | [繁體中文](README_zh-TW.md) | [日本語](README_ja.md)

`codex-hosts` は Codex 向けの Windows SSH / Telnet ホスト管理ツールです。パスワード、秘密鍵のパスフレーズ、FIDO PIN などの機密情報を会話・コマンド引数・リクエストファイルで直接扱わずに、Codex からリモートホストへ接続して操作できます。

![Codex Hosts メインウィンドウ](../../Main.png)

## 主な機能

- 再利用可能な SSH / Telnet ホスト設定を管理。
- パスワード認証と通常の OpenSSH 秘密鍵認証に対応。
- FIDO / YubiKey などのハードウェアセキュリティキーに対応。
- Windows OpenSSH Agent または Pageant に読み込まれている ID に対応。
- SSH ホストキーのフィンガープリント固定とユーザー確認。
- 検証済み SSH Jump Host / 踏み台チェーンに対応。
- 単一ホストでのコマンド実行。
- 1 本の SSH 接続上で複数コマンドを並行実行。
- SSH / Telnet の複数ホストをまとめて疎通確認・実行。
- API Key、Token など Codex に直接渡したくない値をメモリ上だけで保持する一時シークレット機能。
- ホスト検索、接続、認証、コマンド実行を自動化する完全な Codex Skill。

## インストール

### Release をダウンロード

ビルド済みバージョンは 64 ビット版 Windows 10 以降に対応しています。[Releases](https://github.com/Torinomii/codex-hosts/releases/latest) からダウンロードできます。

### Codex Skill をインストール

インストール後の Skill は次の構成になります：

```text
%USERPROFILE%\.codex\skills\codex-hosts\
├── SKILL.md
├── bin\
│   └── codex-hosts.exe
├── agents\
└── references\
```

手動でインストールする場合：

1. Release 内の `codex-hosts.exe` を次の場所に配置します：

```text
%USERPROFILE%\.codex\skills\codex-hosts\bin\codex-hosts.exe
```

2. Release 内の `skill\codex-hosts` の内容をすべて次の場所へコピーします：

```text
%USERPROFILE%\.codex\skills\codex-hosts
```

`SKILL.md` または実行ファイルだけをコピーせず、Skill ディレクトリ全体を保持してください。

Codex にインストールを依頼することもできます：

```text
https://github.com/Torinomii/codex-hosts/releases/latest から最新版の codex-hosts をダウンロードしてインストールしてください。
現在の環境の Skill インストールディレクトリを自動的に特定し、完全な Skill と実行ファイルをインストールして、必要なファイルがすべて配置されていることを確認してください。
```

### ソースからビルド

Rust 1.92 以降と MSVC ツールチェーンが必要です：

```powershell
git clone https://github.com/Torinomii/codex-hosts.git
cd codex-hosts
cargo build --locked --release
```

生成される実行ファイル：

```text
target\release\codex-hosts.exe
```

ビルド後、この実行ファイルを Skill の `bin` ディレクトリに配置します。

## クイックスタート

### 1. ホストを追加

`codex-hosts.exe` を開き、新しいホストを作成します。

入力項目：

- エイリアス
- アドレスまたは IP
- ポート
- ユーザー名
- プロトコル
- 認証方式

入力後に保存します。

### 2. 認証方式を選択

| 認証方式 | 説明 |
| --- | --- |
| Password | パスワード認証。パスワードは Windows 資格情報マネージャーに保存 |
| OpenSSH Key | 通常の OpenSSH 秘密鍵ファイルを使用 |
| FIDO / セキュリティキー | OpenSSH FIDO Handle を使用し、ハードウェア側で署名 |
| SSH Agent | Windows OpenSSH Agent または Pageant に読み込まれている ID を使用 |

これらの認証方式で使う機密値は Codex の会話を経由しません。

### 3. Codex からホストを使う

ホストを保存したら、Codex にホストのエイリアスと実行したい作業を伝えるだけです。

例：

```text
example に接続して hostname を実行してください。
```

または：

```text
web-1 と web-2 のディスク使用量を確認してください。
```

複数の操作をまとめて依頼することもできます：

```text
server1 に接続して hostname、uptime、ディスク容量を確認してください。
```

Codex Skill が次を処理します：

- ホスト設定の検索
- 接続の確立
- SSH Host Key の検証
- 認証
- コマンド実行
- 構造化された結果の返却

通常の利用では Tool JSON を手動で書く必要はありません。

## セキュリティ境界

### ログイン資格情報

パスワードと秘密鍵ファイルのパスフレーズは Windows 資格情報マネージャーに永続保存されます。

これらの機密値は：

- ホスト設定へ書き込まれません
- Tool JSON に含まれません
- コマンドライン引数として渡されません
- Codex に返されません

FIDO PIN は現在の操作にだけ使用され、保存されません。

### SSH Host Key

SSH ホストのフィンガープリントはユーザーが明示的に確認する必要があります。

未知のホストへ初めて接続すると、`codex-hosts` は実際に検出したホストフィンガープリントを表示します。

ユーザーが確認した後にだけ保存されます。

その後サーバーの Host Key が変更された場合も、保存済みのフィンガープリントを自動で置き換えることはありません。再度ユーザー確認が必要です。

### 一時シークレット

`codex-hosts` は、ホストログインとは無関係な API Key、Token などの機密値を一時的に保持することもできます。

これらの値は現在実行中の `codex-hosts` プロセスのメモリ内だけに保持されます。Windows 資格情報マネージャーには保存されず、Codex に返されることもありません。

ユーザー承認後、指定したプログラムの環境変数へ直接注入できます。

`codex-hosts` の終了、Windows のサインアウト、またはシステム再起動で一時値は失効します。

詳細は [`temporary-secrets.md`](../../skill/codex-hosts/references/temporary-secrets.md) を参照してください。

## FIDO / セキュリティキー

`codex-hosts` は既存の OpenSSH ECDSA-SK および Ed25519-SK FIDO Handle を利用できます。

例：

```text
id_ecdsa_sk
id_ed25519_sk
```

アプリ内で新しい FIDO SSH 資格情報を作成または復元する場合は ECDSA-SK を使用します。

FIDO モードでは：

- ハードウェア秘密鍵はセキュリティデバイスの外へ出ません。
- FIDO PIN は現在の操作にだけ使用され、保存されません。
- SSH Agent は不要です。
- Agent Forwarding は常に無効です。
- セキュリティキーの設定によっては PIN または Touch が必要です。

FIDO Handle と SSH Agent は別の認証経路です。

FIDO Handle：

```text
codex-hosts
    │
    ▼
OpenSSH FIDO Handle
    │
    ▼
セキュリティキー
```

`codex-hosts` がシステムコンポーネントを通して直接ハードウェア署名を行います。

SSH Agent：

```text
codex-hosts
    │
    ▼
OpenSSH Agent / Pageant
    │
    ▼
読み込み済み ID
```

認証はすでに実行中の Agent に委譲されます。

`codex-hosts` が Agent サービスを自動的に起動、有効化、永続化することはありません。

## Jump Host

SSH ホストは、保存済みかつ検証済みの別の SSH ホストを Jump Host として利用できます。

例：

```text
Codex
  │
  ▼
jump-1
  │
  ▼
jump-2
  │
  ▼
target
```

セキュリティルール：

- Jump Host は検証済み SSH ホストである必要があります。
- すべての Hop で Host Key を検証します。
- Jump Host チェーンの循環は拒否されます。
- 1 本の SSH チェーンは最大 8 ホストです。
- Agent Forwarding は常に無効です。

## Telnet

`codex-hosts` は Telnet にも対応しています。

ただし Telnet 自体には暗号化がなく、ユーザー名、パスワード、コマンド、返却データが平文でネットワーク上を流れる可能性があります。

このリスクを明示的に許容できる信頼済みネットワークでのみ使用してください。

インターネット経由の接続には SSH を推奨します。

## バッチ実行

### 1 台の SSH ホストで複数コマンド

`exec_many` は 1 本の SSH 接続を再利用し、複数の独立した短いコマンドを並行実行できます：

```json
{
  "action": "exec_many",
  "alias": "example",
  "commands": [
    "hostname",
    "uptime",
    "df -h"
  ],
  "max_concurrency": 8
}
```

SSH 認証は 1 回だけ行い、その後同じ接続上で複数の Channel を開きます。

FIDO / セキュリティキーでは、複数コマンドが通常 1 回の認証を共有できるため特に有効です。

`exec_many` は SSH ホスト専用です。

### 複数ホストをまとめて実行

対象ホストを明示的に指定できます：

```json
{
  "action": "batch_exec",
  "aliases": [
    "web-1",
    "web-2",
    "web-3"
  ],
  "command": "uptime",
  "max_concurrency": 8,
  "batch_timeout_ms": 30000
}
```

複数ホストの疎通確認もできます：

```json
{
  "action": "batch_probe",
  "aliases": [
    "web-1",
    "web-2"
  ],
  "max_concurrency": 8,
  "batch_timeout_ms": 30000
}
```

バッチモードではホスト一覧を明示的に指定する必要があります。

空の一覧が「全ホスト」として扱われることはありません。

<details>
<summary>Codex / Tool インターフェース</summary>

### GUI 編集モード

Codex またはスクリプトからホストエディターを開き、機密情報を含まない接続情報を初期値として渡せます：

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

パスワード、秘密鍵のパスフレーズ、FIDO PIN を引数として渡さないでください。

認証引数には固定名を使用します：

- `password`：パスワード認証。
- `private-key` / `private_key`：通常の秘密鍵ファイルまたは FIDO Handle。`fido-handle` も使用できます。
- `ssh-agent` / `ssh_agent`：Windows OpenSSH Agent または Pageant。

`private-key` は通常の OpenSSH 秘密鍵と FIDO Handle の両方に使用されます。

`ssh-agent` は Agent / Pageant モードだけを表し、すべてのハードウェアキー認証を意味するものではありません。

### Tool モード

Tool モードは UTF-8 JSON のリクエストファイルと結果ファイルを使用します。どちらにも資格情報を含めてはいけません。

主なリクエスト：

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

`agent_identities` と `fido_identities` が返すのは公開 ID 情報と公開鍵だけです。

リモートコマンドの実行結果では `output_truncated` を確認し、サイズ制限によって出力が省略されていないか確認してください。

Codex の詳細な動作、呼び出し手順、安全ルールは [`SKILL.md`](../../skill/codex-hosts/SKILL.md) を参照してください。

</details>

## 実行制限

異常なコマンドによる過剰な出力やメモリ使用を防ぐため、実行には制限があります。

- 1 コマンドで取得できる出力は最大 **1 MiB**。
- 完成した `exec_many` またはバッチ JSON 結果は最大 **8 MiB**。
- `exec_many` とバッチ処理は同時実行数を制限します。
- 出力が省略された場合は `output_truncated` で確認できます。
- ネットワーク障害が発生してもリモートコマンドを自動再実行しません。

自動再試行を行わないのは、リモートコマンドが冪等とは限らないためです。例：

```text
reboot
rm
systemctl restart
データベースへの書き込み
デプロイ操作
```

ネットワーク障害が発生しても、同じコマンドをもう一度実行して安全とは限りません。

## プロジェクト構成

```text
codex-hosts
├── src\
├── skill\
│   └── codex-hosts\
│       ├── SKILL.md
│       ├── agents\
│       └── references\
├── languages\
├── docs\
│   └── readme\
│       ├── README_zh-CN.md
│       ├── README_zh-TW.md
│       └── README_ja.md
├── Cargo.toml
├── Cargo.lock
├── Main.png
└── README.md
```

## License

本プロジェクトは [Apache License 2.0](../../LICENSE) の下で提供されます。
