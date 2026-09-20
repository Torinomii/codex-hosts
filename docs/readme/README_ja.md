# codex-hosts

<p align="center">
  <a href="https://github.com/Torinomii/codex-hosts/actions/workflows/release.yml"><img src="https://github.com/Torinomii/codex-hosts/actions/workflows/release.yml/badge.svg" alt="Release ビルド" /></a>
  <a href="../../LICENSE"><img src="https://img.shields.io/badge/License-Apache--2.0-D22128.svg" alt="Apache 2.0 ライセンス" /></a>
  <img src="https://img.shields.io/badge/Windows-10%2B-0078D4?logo=windows&logoColor=white" alt="Windows 10 以降" />
  <img src="https://img.shields.io/badge/Rust-1.92%2B-000000?logo=rust&logoColor=white" alt="Rust 1.92 以降" />
  <a href="https://linux.do/"><img src="https://img.shields.io/badge/LINUX-DO-FFB003" alt="LINUX DO コミュニティ" /></a>
</p>

[English](../../README.md) | [简体中文](README_zh-CN.md) | [繁體中文](README_zh-TW.md) | [日本語](README_ja.md)

`codex-hosts` は Codex 向けの Windows SSH / Telnet ホスト管理ツールです。MCP サーバーとして動作し、Codex は構造化されたツールを通じてリモートホストへ接続・操作します。パスワード、秘密鍵のパスフレーズ、FIDO PIN などの機密情報は会話・コマンド引数・ファイルに一切現れません。

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
- リスク注釈付きツールを備えた MCP サーバーと、ホスト検索、接続、認証、コマンド実行のための Codex Skill。
- ホストごとの認証の保持：操作ごとに再認証（既定）、Codex セッション中は保持、またはアイドル後に切断。

## 0.3.1 の変更点

- MCP サーバー：`codex-hosts.exe --mcp` が stdio 経由で Codex にサービスを提供します。ホスト、プローブ、コマンド、バッチ、ホストエディター、一時シークレットはすべてリスク注釈付きの MCP ツールになり、シェルや PowerShell ラッパー、一時リクエストファイルは不要になりました。従来のファイル方式の Tool モードはそのまま残り、未更新の Skill でも動作します。
- 認証の保持：各ホストで、Codex が操作ごとに再認証する（既定、従来の動作）か、Codex セッション中はセッションを保持するか、指定したアイドル時間後に切断するかを選択します。セッションを保持するホストは一覧にマークが付き、エディターに警告が表示されます。
- エディター：必須項目に `*` が付き、未入力のまま保存すると強調表示されます。新しい「詳細」カードには、ホストごとの同時チャネル数、既定タイムアウト、キープアライブ間隔、Codex 向けのリモート環境ヒント、Codex からの非表示、Telnet プロンプトの上書きが含まれます。
- Codex の呼び出しをキャンセルすると待機を停止しチャネルを閉じます。長時間の作業は長い待機ではなく、リモートホスト自身の `tmux` / `screen` を使う手順として文書化しました。
- `codex-hosts.exe --version` がバージョンを表示し、引数エラーは stderr に出力されます。

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

3. `%USERPROFILE%\.codex\config.toml`（またはワークスペースの `.codex\config.toml`）に MCP サーバーを登録します：

```toml
[mcp_servers.codex-hosts]
command = "C:\\Users\\<user>\\.codex\\skills\\codex-hosts\\bin\\codex-hosts.exe"
args = ["--mcp"]
startup_timeout_sec = 20
tool_timeout_sec = 86400
```

`tool_timeout_sec` は上限にすぎず、各呼び出しは独自のタイムアウトを持ちます。長時間ジョブでブロッキング待機を使わないなら Codex の既定値のままで構いません。

`SKILL.md` または実行ファイルだけをコピーせず、Skill ディレクトリ全体を保持してください。

Codex にインストールを依頼することもできます：

```text
https://github.com/Torinomii/codex-hosts/releases/latest から最新版の codex-hosts をダウンロードしてインストールしてください。
現在の環境の Skill インストールディレクトリを自動的に特定し、完全な Skill と実行ファイルをインストールし、SKILL.md の説明に従って config.toml に codex-hosts MCP サーバーを登録して、必要なファイルがすべて配置されていることを確認してください。
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

### ローカルソースチェックアウトをリンクする

固定パスのローカルチェックアウトで開発する場合は、ビルドのたびにコピーせず、シンボリックリンクでインストールできます。

```powershell
pwsh -NoProfile -File .\scripts\install-local-skill.ps1
```

このスクリプトは、インストール先の `SKILL.md`、`agents`、`references` を `skill\codex-hosts` にリンクし、`bin\codex-hosts.exe` を `target\release\codex-hosts.exe` に直接リンクします。既存インストールを置き換える前にすべてのソースを検証し、完了後にリンク先を確認します。失敗した場合は以前のインストールを復元します。同じ構成を確認するため、いつでも再実行できます。現在の Windows ユーザーにシンボリックリンクを作成する権限が必要です。

このリンク方式はローカルソースチェックアウト専用です。ダウンロードした Release アーカイブでは、上記のコピー方式を引き続き使用してください。

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
- 説明とタグ（任意）

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

通常の利用ではツールを手動で呼び出す必要はありません。

## セキュリティ境界

### ログイン資格情報

パスワードと秘密鍵ファイルのパスフレーズは Windows 資格情報マネージャーに永続保存されます。

これらの機密値は：

- ホスト設定へ書き込まれません
- MCP ツールのパラメーターや結果に含まれません
- コマンドライン引数として渡されません
- Codex に返されません

FIDO PIN は現在の操作にだけ使用され、保存されません。

### SSH Host Key

SSH ホストのフィンガープリントはユーザーが明示的に確認する必要があります。

未知のホストへ初めて接続すると、`codex-hosts` は実際に検出したホストフィンガープリントを表示します。

ユーザーが確認した後にだけ保存されます。

その後サーバーの Host Key が変更された場合も、保存済みのフィンガープリントを自動で置き換えることはありません。再度ユーザー確認が必要です。

### 認証の保持

既定では Codex の呼び出しごとに再認証するため、ハードウェアキーは操作ごとに一度タッチが必要で、Codex は呼び出しの間にセッションを保持しません。各ホストの「認証」カードでこれを緩和できます：

| オプション | 動作 |
| --- | --- |
| 操作ごとに再認証する | 既定。呼び出し終了時に接続を閉じます。 |
| Codex セッション中は保持する | 認証済みセッションをキープアライブ付きで開いたままにし、Codex の終了、リンク切断、または `disconnect` の呼び出しまで保持します。 |
| アイドル後に切断する | 指定した分数アイドル状態が続くまでセッションを保持します。 |

セッションを保持している間、そのホストへの Codex の後続コマンドは再認証もセキュリティキーのタッチも求めず、「操作ごとに一度確認する」保護が失われます。エディターはこの警告を表示し、セッションを保持するホストは一覧にマークが付き、Codex にはこの設定の変更を提案しないよう指示しています。Telnet ホストは常に再認証します。資格情報、PIN、ホスト鍵の確認は影響を受けず、再利用されるのは有効なセッションだけです。

### 一時シークレット

`codex-hosts` は、ホストログインとは無関係な API Key、Token などの機密値を一時的に保持することもできます。

これらの値は現在実行中の `codex-hosts` プロセスのメモリ内だけに保持されます。Windows 資格情報マネージャーには保存されず、Codex に返されることもありません。

ユーザー承認後、指定したプログラムの環境変数へ直接注入できます。

`codex-hosts` トレイアプリの終了、Windows のサインアウト、またはシステム再起動で一時値は失効します。Codex の起動や終了は影響しません。

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
<summary>MCP ツール</summary>

Codex は stdio 経由で `codex-hosts.exe --mcp` と通信します。各ツールは同じ JSON を `structuredContent` とテキストの両方で返し、ポリシー層が読み取り専用ツールとコードを実行するツールを区別できるよう MCP 注釈を持ちます。

| ツール | 用途 |
| --- | --- |
| `list_hosts` | 保存済みホストの非機密情報、信頼状態、`auth_persistence`、`max_channels`、`remote_env` |
| `agent_identities`、`fido_identities` | 読み込まれた Agent 鍵と FIDO Handle の公開情報 |
| `probe`、`batch_probe` | 認証して `hostname` を実行し、未知または変更されたホスト鍵を明らかにする |
| `exec`、`exec_stdin`、`exec_many`、`batch_exec` | コマンドを実行する。`exec_stdin` は stdin のプログラム本文がレビューに対して不透明なため別ツール |
| `disconnect` | セッション保持を選択したホストのために保持している接続を切断する |
| `open_host_editor` | 非機密情報を事前入力したエディターウィンドウを開く、または報告されたホスト鍵を確認する |
| `temporary_secrets_open`、`_status`、`_run`、`_clear` | トレイアプリが保持するメモリ限定シークレット |

パラメーターにパスワード、パスフレーズ、PIN が含まれることはなく、エディターがマスク付き入力欄で受け取ります。タイムアウト（`connect_timeout_ms`、`command_timeout_ms`、`batch_timeout_ms`）は呼び出しごとに指定し、省略時は接続 120 秒、コマンド 10 分、またはホストの「詳細」カードの既定値が使われます。

`open_host_editor` の認証引数は安定した名前を使います：`password`、通常の秘密鍵ファイルまたは FIDO Handle には `private-key` / `private_key`（`fido-handle` も可）、Windows OpenSSH Agent または Pageant には `ssh-agent` / `ssh_agent`。

リモートコマンドを実行したら、サイズ制限で出力が省略されていないか `output_truncated` を確認してください。

Codex の完全な動作、ワークフロー、安全規則は [`SKILL.md`](../../skill/codex-hosts/SKILL.md) を参照してください。

</details>

## ホストの説明・タグ・コマンド入力

ホストに複数行の `description` と複数の `tags` を保存できます。編集画面でタグを追加・削除できます。前後の空白、空のタグ、大文字小文字を区別しない重複は除去し、最初の表記を保持します。説明・タグだけの変更では接続確認やホスト鍵の信頼状態は維持されます。検索対象はエイリアス、アドレス、ユーザー名、説明、タグです。複数のタグ条件はすべて一致する必要があります。一括全選択は表示中のホストだけが対象で、フィルター変更時は非表示ホストの選択を解除します。

`list_hosts` は両フィールドを返し、任意の `tags` 配列で絞り込めます。省略または空配列は全ホスト、存在しないタグは空一覧になります。`open_host_editor` は `description` と `tags` を事前入力できます。省略した値は保持し、空の説明や単独の空タグで消去できます。CSV テンプレート・インポート・エクスポートは任意の `description`、`tags` 列に対応します。タグのセルは `["prod","web"]` のような JSON 配列を CSV の規則で引用します。従来の設定と CSV も利用できます。

```json
{"tags":["prod","web"]}
{"alias":"example","command":"python3 -","stdin":"print('hello')\n","command_timeout_ms":10000}
```

`exec_stdin` は単一の SSH ホストのコマンドに最大 1 MiB の UTF-8 テキストを、改行を追加せずそのまま送信し、最後に EOF を送ります。`""` は即座に EOF を送ります。入力送信と出力受信は同時に進み、既存のタイムアウトと出力制限が適用されます。リモートプログラムが全入力を読む前に終了した場合、その終了状態を優先します。入力超過は `STDIN_TOO_LARGE`、Telnet ホストでは `STDIN_UNSUPPORTED` になります。コマンドは自動再実行されません。

## 実行制限

異常なコマンドによる過剰な出力やメモリ使用を防ぐため、実行には制限があります。

- 1 コマンドで取得できる出力は最大 **1 MiB**。
- 完成した `exec_many` またはバッチ JSON 結果は最大 **8 MiB**。
- `exec_many` とバッチ処理は同時実行数を制限します。
- 出力が省略された場合は `output_truncated` で確認できます。
- ネットワーク障害が発生してもリモートコマンドを自動再実行しません。
- Codex の呼び出しをキャンセルするとチャネルを閉じて `CANCELLED` を返します。すでに開始したリモートコマンドは実行を続ける場合があります。
- 数分を超える作業はリモートホストの `tmux` / `screen` で行ってください。[`long-running.md`](../../skill/codex-hosts/references/long-running.md) を参照。

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
