<h1 align="center">codex-hosts</h1>

<p align="center">保存したホストへ。資格情報は手元に。作業は Codex に。</p>

<p align="center">
  <a href="https://github.com/Torinomii/codex-hosts/tree/v0.4.0"><img src="https://img.shields.io/badge/version-0.4.0-2563eb" alt="v0.4.0" /></a>
  <a href="https://github.com/Torinomii/codex-hosts/actions/workflows/release.yml"><img src="https://github.com/Torinomii/codex-hosts/actions/workflows/release.yml/badge.svg" alt="Build release" /></a>
  <a href="../../LICENSE"><img src="https://img.shields.io/badge/License-Apache--2.0-D22128.svg" alt="Apache 2.0 ライセンス" /></a>
  <img src="https://img.shields.io/badge/Windows-10%2B-0078D4?logo=windows&logoColor=white" alt="Windows 10 以降" />
  <a href="https://linux.do/"><img src="https://img.shields.io/badge/LINUX-DO-FFB003" alt="LINUX DO Community" /></a>
</p>

<p align="center">
  <a href="../../README.md">English</a> · <a href="README_zh-CN.md">简体中文</a> · <a href="README_zh-TW.md">繁體中文</a> · <a href="README_ja.md">日本語</a>
</p>

<p align="center">
  <a href="https://github.com/Torinomii/codex-hosts/releases">ダウンロード</a> · <a href="#インストール">インストール</a> · <a href="#クイックスタート">クイックスタート</a> · <a href="#リリース履歴">リリース履歴</a> · <a href="#技術リファレンス">技術リファレンス</a>
</p>

Codex 向けの Windows SSH / Telnet ホスト管理ツール兼 MCP サーバーです。ホストをローカル、Bitwarden、1Password のいずれかに保存すれば、接続、コマンド実行、複数マシンでの作業を Codex に依頼できます。パスワード、鍵のパスフレーズ、FIDO PIN はローカルで入力し、会話やツール引数には含めません。

<p align="center">
  <img src="../../Main.png" alt="ローカル、Bitwarden、1Password のホストを表示した Codex Hosts メインウィンドウ" width="960" />
</p>

## 機能一覧

| 機能 | できること |
| --- | --- |
| ホスト管理 | 説明やタグ付きの SSH / Telnet ホストを保存し、検索、絞り込み、インポート、エクスポート。 |
| 資格情報の保存先 | ログイン情報を Windows 資格情報マネージャー、Bitwarden のフォルダー、1Password の保管庫に保存。 |
| 認証方式 | パスワード、OpenSSH 秘密鍵、FIDO / YubiKey、Windows OpenSSH Agent、Pageant。 |
| リモートコマンド | コマンド実行、stdin の送信、sudo / su で明示したユーザーとしてプログラムを実行。 |
| 並行・バッチ実行 | 1 本の SSH 接続で複数コマンドを実行、または明示したホスト一覧に対して実行。 |
| 接続の管理 | ホスト鍵と踏み台チェーンを検証。毎回の再認証、セッション保持、アイドル切断を選択。 |
| 呼び出し承認 | SSH の呼び出しを接続前に毎回確認可能。 |
| 一時シークレット | API Key や Token をメモリだけに保持し、ローカルプログラムの環境変数に注入。 |

## 0.4.0 の主な変更点

- **Bitwarden ホスト：** 選んだフォルダーのログインアイテムがすべてホストになります（名前 = エイリアス、`ssh://` / `telnet://` URI = 接続先、ユーザー名とパスワード = ログイン）。使うたびに公式 `bw` CLI で読み直すため、保管庫での変更は次の呼び出しから反映されます。
- **1Password ホスト：** 選んだ 1Password 保管庫のログインアイテムも同様にホストになり、公式 `op` CLI と 1Password デスクトップアプリ経由で読み取ります。ロック解除は 1Password 側で行います。
- **ウィンドウで管理：** 保管庫ホストもローカルホストと同じエディターで作成、編集、削除できます。踏み台、呼び出し承認、詳細設定は `codex-hosts` に保存されます。

[変更を比較](https://github.com/Torinomii/codex-hosts/compare/v0.3.3...v0.4.0)

## インストール

最も手早いのは Codex に依頼する方法です。次の内容を Codex に貼り付けてください。

```text
https://github.com/Torinomii/codex-hosts.git を固定ディレクトリにクローンし、scripts/install-local-skill.ps1 でビルド環境を確認してください。ビルド、リンクのインストール、旧パスの置き換えはそれぞれ実行前に私に確認してください。
インストール先の codex-hosts Skill ディレクトリ全体が単一のシンボリックリンクで、実行ファイルがリリースビルドを参照することを確認し、SKILL.md に従って config.toml に codex-hosts MCP サーバーを登録し、チェックアウトを保持してください。
```

Codex がリポジトリをクローンして対話式インストーラーを実行し（コンパイル、リンク作成、置き換えの前に確認を求めます）、MCP サーバーを登録します。ソースからビルドするため、下表の「ソースからビルド」と同じ環境が必要です。手動でインストールする場合は、表の後の 2 つの手順に従ってください。

| 方法 | 必要な環境 | 開始方法 |
| --- | --- | --- |
| Codex に依頼（推奨） | Codex + 「ソースからビルド」の環境 | 上のプロンプト |
| GitHub Release | 64 ビット Windows 10+、PowerShell 7.2+ | `install.cmd` または以下のコマンド |
| ソースからビルド | 上記 + Rust 1.92+、MSVC、Windows SDK | `scripts/install.cmd` または `scripts/install-local-skill.ps1` |

### 1. パッケージをインストール

[Releases ページ](https://github.com/Torinomii/codex-hosts/releases)から `codex-hosts-windows-x86_64.zip` をダウンロードし、同じ場所の `.sha256` と照合してから（`(Get-FileHash .\codex-hosts-windows-x86_64.zip).Hash`）、固定のディレクトリに展開して実行します。

```powershell
pwsh -NoProfile -File .\install-local-skill.ps1 -ReleasePackage
```

`install.cmd` をダブルクリックしても同じ対話式インストーラーが起動します。環境を確認し、以前のインストールを置き換える前に確認を求めます。

**展開したディレクトリは移動しないでください。** インストールされた Skill はそこへのシンボリックリンクのため、現在のユーザーにシンボリックリンクの作成が許可されている必要があります。

### 2. MCP サーバーを登録

`%USERPROFILE%\.codex\config.toml`（またはワークスペースの `.codex\config.toml`）に以下を追加し、`<user>` を Windows のアカウント名に置き換えます。

```toml
[mcp_servers.codex-hosts]
command = "C:\\Users\\<user>\\.codex\\skills\\codex-hosts\\bin\\codex-hosts.exe"
args = ["--mcp"]
startup_timeout_sec = 20
tool_timeout_sec = 86400
```

<details>
<summary><strong>ソースからビルド</strong></summary>

```powershell
git clone https://github.com/Torinomii/codex-hosts.git
cd codex-hosts
pwsh -NoProfile -File .\scripts\install-local-skill.ps1
```

スクリプトは PowerShell、Rust、MSVC ツールチェーン、Windows SDK を確認し、不足があれば説明したうえで、コンパイル前とリンク作成前に確認を求めます。インストールされた Skill は `skill\codex-hosts` に、Skill の `bin\codex-hosts.exe` はビルド結果にリンクされるため、再ビルドするとインストール済みのプログラムも更新されます。置き換えたパスは `.codex-hosts.backup-*` として残ります。

手動でビルドする場合は `cargo build --locked --release` の後、`-SkipBuild` を付けてインストールします。

シンボリックリンクを作成できない場合は、開発者モードを有効にするか管理者権限の PowerShell を使ってください。スクリプトがシステム設定を変更することはありません。

| オプション | 効果 |
| --- | --- |
| `-CodexHome <directory>` | `CODEX_HOME` / `%USERPROFILE%\.codex` の代わりに別の Codex ホームへインストール |
| `-NonInteractive -Build -Install` | 無人でビルドとインストール。以前のインストールを置き換えるには `-ReplaceExisting` を追加 |
| `-SkipBuild` / `-SkipInstall` | 既存のビルドを使用 / ビルドのみ |

</details>

## クイックスタート

### 1. ホストを追加

`codex-hosts.exe` を開いて「ホストを追加」を選び、エイリアス、アドレス、ポート、ユーザー名、プロトコル、認証方式を入力して保存します。

| 認証方式 | 使うもの |
| --- | --- |
| Password | Windows 資格情報マネージャー、または Bitwarden / 1Password のアイテムに保存したパスワード |
| OpenSSH Key | OpenSSH 秘密鍵ファイル |
| FIDO / セキュリティキー | ハードウェアキー上の OpenSSH FIDO Handle |
| SSH Agent | Windows OpenSSH Agent または Pageant に読み込まれた ID |

### 2. 任意：Bitwarden または 1Password にホストを置く

ツールバーの「資格情報サービス」を開き、「設定を開始…」を選びます。設定後は「ホストを追加」からそのサービスのホストも作成できます。

| サービス | 必要なもの | ホストになるもの |
| --- | --- | --- |
| Bitwarden | 公式 Bitwarden CLI（`bw.exe`）、bitwarden.com、bitwarden.eu または独自サーバーのアカウント | 1 つのフォルダーのログインアイテム |
| 1Password | 「設定 › 開発者 › 1Password CLI と連携」をオンにした 1Password デスクトップアプリと公式 1Password CLI（`op.exe`） | 1 つの保管庫のログインアイテム。専用の保管庫を推奨 |

### 3. Codex からホストを使う

保存したエイリアスと作業内容を伝えます。

```text
example に接続して hostname を実行して。
web-1 と web-2 のディスク使用量を確認して。
```

Skill がホストを調べ、ホスト鍵を検証して認証し、構造化された結果を返します。MCP ツールを自分で呼び出す必要はありません。

## セキュリティ境界

| 制御 | 動作 |
| --- | --- |
| ログイン資格情報 | Windows 資格情報マネージャー、またはホストの Bitwarden / 1Password アイテムに保存。ホスト設定、ツール引数、コマンドライン、結果には含めません。FIDO PIN は保存しません。 |
| パスワードマネージャー | Bitwarden のマスターパスワードは `codex-hosts` 内でのみ入力し、1Password のロック解除は 1Password アプリだけで行います。 |
| ホストの身元 | 未知または変更された SSH ホスト鍵には確認が必要で、踏み台の各 Hop も同様です。 |
| セッション保持 | 保持中のセッションでは後続の認証とキーのタッチを省略します。呼び出し承認は別の設定です。 |
| sudo / su | 実行ユーザーは呼び出しごとに指定。保存パスワードの供給には個別の許可が必要です。 |
| 一時シークレット | メモリ内だけに保持し、初期状態でフィールドごとに承認が必要。トレイアプリの終了で消去します。 |

> **Telnet は暗号化されません。** 資格情報、コマンド、出力が平文で送信される可能性があります。信頼できるネットワークでのみ使用し、SSH を優先してください。

<details>
<summary><strong>資格情報の保存とパスワードマネージャー</strong></summary>

ローカルホストのパスワードと鍵のパスフレーズは、サインイン中の Windows アカウントの資格情報マネージャーに保存します。FIDO PIN はその操作の間だけ使います。

ホストストア（`hosts.json`）は `codex-hosts` を実行する Windows アカウントが所有している必要があります。そうでない場合、MCP サーバーは起動せず、修復用の `icacls /setowner` コマンドをログに出力します。ウィンドウは警告のみ表示します。

Bitwarden / 1Password のホストはそのログインアイテムそのものです。エイリアス、接続先、ユーザー名、パスワード、説明、タグはアイテムに、その他の設定は `codex-hosts` に保存されます。ウィンドウでホストを保存するとアイテムに書き込み、削除すると Bitwarden のアイテムはゴミ箱へ、1Password のアイテムはアーカイブへ移るため、どちらも復元できます。保管庫側でその間に変更されたアイテムを上書きすることはありません。

- **Bitwarden：** 「再認証」で保管庫のロック解除を保つ期間を選びます（毎回、指定時間アイドルになるまで、`codex-hosts` を閉じるまで、ロックするまで）。保持中のセッションキーは Windows 資格情報マネージャーに保存し、終了時には `bw lock` も実行します。
- **1Password：** `codex-hosts` がアカウントのパスワードを扱うことはありません。1Password アプリが codex-hosts の許可を求め、その有効期間も 1Password が決めます。

</details>

<details>
<summary><strong>ホスト鍵と認証の保持</strong></summary>

初回接続時、`codex-hosts` はサーバーのホスト鍵フィンガープリントを表示し、確認後にのみ保存します。変更された鍵が自動で置き換えられることはありません。

各ホストの「認証」カードで、Codex が認証済みセッションを保持する期間を選びます。

| オプション | 動作 |
| --- | --- |
| 操作ごとに再認証する | 呼び出し終了時に接続を閉じます。旧バージョンのホストとインポートしたホストはこの設定です。 |
| Codex セッション中は保持する | 新規ホストで提案される設定。Codex の終了、リンク切断、または `disconnect` の呼び出しまで保持します。 |
| アイドル後に切断する | 指定した分数アイドルが続くとセッションを閉じます。 |

セッションを保持している間、そのホストの後続コマンドは再認証もセキュリティキーのタッチもなしに実行されます。エディターは初回保存時と変更時にこの選択の確認を求め、該当するホストは一覧に印が付きます。Telnet ホストは常に再認証します。

</details>

<details>
<summary><strong>一時シークレット</strong></summary>

`codex-hosts` はホストのログインとは無関係な API Key や Token などの値も保持できます。値は `codex-hosts` のプロセスメモリだけに置かれ、Codex には返されず、Codex が実行するローカルプログラムの環境変数に注入されます。

各フィールドは初期状態で承認が必要で、それを使うプログラムの起動前にダイアログで確認します。フィールドごとに承認をオフにすると直接実行できます。値はトレイアプリの終了、サインアウト、Windows の再起動で失効し、Codex の再起動では消えません。

詳細は [`temporary-secrets.md`](../../skill/codex-hosts/references/temporary-secrets.md) を参照してください。

</details>

## リリース履歴

<details>
<summary><strong>0.4.0 · Bitwarden と 1Password のホスト</strong></summary>

- **Bitwarden ホスト：** 1 つのフォルダーのログインアイテムがホストになり、説明とタグは `codex-hosts:` カスタムフィールドに保存されます。使うたびに、指定した期間より古くない同期の後で公式 `bw` CLI がアイテムを読み直し、フォルダーで作成したホストはすぐにエイリアスで見つかります。
- **Bitwarden のロック解除：** 保管庫のロック解除を保つ期間を選択（毎回、アイドル時間、終了まで、ロックするまで）。bitwarden.com、bitwarden.eu、独自サーバー、新しいデバイスの確認に対応。
- **1Password ホスト：** 1 つの保管庫のログインアイテムがホストになり、公式 `op` CLI と 1Password デスクトップアプリ経由で読み取ります。設定時に `op.exe` を自動検索し、保管庫の作成もできます。
- **編集：** 保管庫ホストもローカルホストと同じエディターで作成、編集、削除できます。保管庫側で同時に変更された場合は上書きせずに知らせます。
- **Codex：** `list_hosts` が `credential_source` を返し、新しい `BITWARDEN_*` / `ONEPASSWORD_*` エラーコードでユーザーが行うべき操作を伝えます。sudo と su はアイテムから読んだパスワードを再利用します。

[変更を比較](https://github.com/Torinomii/codex-hosts/compare/v0.3.3...v0.4.0)

</details>

<details>
<summary><strong>0.3.3 · sudo / su、呼び出し承認、インストール手順</strong></summary>

- **sudo / su：** `exec_sudo` と `exec_su` が、明示したユーザーとして絶対パスのプログラムを正確な引数で実行。詳細設定の個別スイッチで保存パスワードの供給を許可し、オフでもパスワード不要の実行は可能です。
- **呼び出し承認：** ホストごとの任意スイッチで、疎通確認、ユーザー切り替え、バッチを含む各 SSH 呼び出しを接続前に確認。保持中の接続でも承認は再利用しません。
- **一時シークレット：** フィールドごとの承認スイッチ（初期状態でオン）。保存や消去で待機中の実行を取り消します。
- **資格情報の安全性：** 保存パスワードはアカウントに紐づき、ログインを変更しても別アカウントのパスワードを引き継ぎません。
- **インストール：** Release とソースのどちらもシンボリックリンクのみでインストールし、環境確認とバックアップ付きの対話式スクリプトを使います。

[変更を比較](https://github.com/Torinomii/codex-hosts/compare/v0.3.2...v0.3.3)

</details>

<details>
<summary><strong>0.3.2 · Skill ディレクトリ全体のリンク</strong></summary>

- インストールされた `codex-hosts` Skill がソースの Skill ディレクトリ全体にリンクし、`SKILL.md`、`agents`、`references` をまとめて扱います。

[変更を比較](https://github.com/Torinomii/codex-hosts/compare/v0.3.1...v0.3.2)

</details>

<details>
<summary><strong>0.3.1 · MCP ツール、認証保持、接続の信頼性</strong></summary>

- **MCP サーバー：** `codex-hosts.exe --mcp` が stdio で Codex に応答し、すべてのツールにリスク注釈を付けます。従来のファイル経由のツールモードも引き続き動作します。
- **認証の保持：** ホストごとに、呼び出しごとの再認証、Codex セッション中の保持、アイドル後の切断を選択。
- **エディター：** 必須項目を表示し、「詳細」カードでホストごとの調整（チャネル数、タイムアウト、キープアライブ、リモート環境、Codex からの非表示、Telnet プロンプト）が可能に。
- Codex の呼び出しをキャンセルするとチャネルを閉じます。長時間の作業はリモートホストの `tmux` / `screen` で行います。
- Telnet ログイン、踏み台の後処理、アイドル計時の信頼性を改善。Release に SHA-256 チェックサムを同梱。

[変更を比較](https://github.com/Torinomii/codex-hosts/compare/v0.3.0...v0.3.1)

</details>

<details>
<summary><strong>0.3.0 · ホスト画面の再設計、説明とタグ</strong></summary>

- **説明とタグ：** メモやタグで検索・絞り込み。`list_hosts` が両方を返して `tags` で絞り込み、`exec` が `stdin` を受け付けます。
- **画面の再設計：** サイズ変更できるホスト一覧、グループ化されたホスト詳細、ステータスバーと通知、一括選択、ライト / ダークテーマ。

[変更を比較](https://github.com/Torinomii/codex-hosts/compare/v0.2.3...v0.3.0)

</details>

## 技術リファレンス

<details>
<summary><strong>MCP ツール</strong></summary>

すべてのツールは同じ JSON を `structuredContent` とテキストの両方で返し、読み取り専用かコードを実行するかを示す MCP 注釈を持ちます。

| ツール | 用途 |
| --- | --- |
| `list_hosts` | 保存済みホストの非機密情報、`credential_source`、信頼状態、`auth_persistence`、`require_call_approval`、sudo/su の権限、`max_channels`、`remote_env` |
| `agent_identities`、`fido_identities` | 読み込まれた Agent 鍵と FIDO Handle の公開情報 |
| `probe`、`batch_probe` | 認証して `hostname` を実行し、未知または変更されたホスト鍵を報告 |
| `exec`、`exec_stdin`、`exec_many`、`batch_exec` | コマンド実行。stdin の内容はコマンドのように確認できないため `exec_stdin` は別ツール |
| `exec_sudo`、`exec_su` | 呼び出しごとに指定した `user` として、絶対パスのプログラムを正確な引数で実行 |
| `disconnect` | 保持中のセッションを切断 |
| `open_host_editor` | 非機密情報を入力済みのエディターを開く、または報告されたホスト鍵を確認 |
| `temporary_secrets_open`、`_status`、`_run`、`_clear` | トレイアプリが保持するメモリ内シークレット |

引数にパスワード、パスフレーズ、PIN が含まれることはありません。タイムアウト（`connect_timeout_ms`、`command_timeout_ms`、`batch_timeout_ms`）は呼び出しごとに指定し、既定は接続 120 秒、コマンド 10 分、またはホストの詳細設定の値です。`open_host_editor` の認証方式は `password`、`private-key`（FIDO Handle も含む）、`ssh-agent` です。

**Codex 呼び出しを毎回承認**（詳細設定、初期状態はオフ）は、接続前に呼び出しのホストとコマンドを表示します。1 回の承認は 1 回の呼び出し（そのバッチの全ホストを含む）だけに有効で、保持中の接続でも次の呼び出しでは再度確認します。応答のない確認は 5 分で期限切れになります。[approvals.md](../../skill/codex-hosts/references/approvals.md) を参照してください。

**sudo / su：** Codex は呼び出しごとに実行ユーザーを指定する必要があります（root は `"user":"root"`）。詳細設定の「sudo で保存パスワードを使用」「su で保存パスワードを使用」で、保存パスワードの供給をそれぞれ許可します。sudo は SSH のログインパスワードまたは別に保存したパスワードを、su は対象アカウントのパスワードを使います。オフでもパスワード不要なら実行でき、パスワードを求められると `SUDO_AUTO_AUTH_DISABLED` / `SU_AUTO_AUTH_DISABLED` で失敗します。扱うのは外側の認証だけで、スクリプト内の sudo にパスワードが渡されることはありません。リモート側の要件は [sudo.md](../../skill/codex-hosts/references/sudo.md) と [su.md](../../skill/codex-hosts/references/su.md) を参照してください。

Codex の動作と安全規則の全体は [`SKILL.md`](../../skill/codex-hosts/SKILL.md) を参照してください。

</details>

<details>
<summary><strong>複数コマンドと複数ホスト</strong></summary>

`exec_many` は 1 本の SSH 接続で複数の短いコマンドを並行実行するため、セキュリティキーのタッチは 1 回で済みます。

```json
{"alias":"example","commands":["hostname","uptime","df -h"],"max_concurrency":8}
```

`batch_exec` と `batch_probe` は明示したホスト一覧を受け取ります。空の一覧がすべてのホストを意味することはありません。

```json
{"aliases":["web-1","web-2","web-3"],"command":"uptime","max_concurrency":8,"batch_timeout_ms":30000}
```

`exec_stdin` は正確な UTF-8 テキスト（最大 1 MiB）を 1 台の SSH ホストのコマンドに送り、EOF で終えます。

```json
{"alias":"example","command":"python3 -","stdin":"print('hello')\n","command_timeout_ms":10000}
```

</details>

<details>
<summary><strong>説明、タグ、CSV</strong></summary>

ホストには複数行の `description` と任意の数の `tags` を保存できます。検索はエイリアス、アドレス、ユーザー名、メモ、タグを対象とし、複数のタグで絞り込むとすべてに一致するホストだけが残ります。`list_hosts` は両方を返して `tags` で絞り込め、`open_host_editor` で入力済みにもできます。

CSV のインポートとエクスポートには任意の `description` 列と `tags` 列があり、タグのセルは `["prod","web"]` のような JSON 配列です。

</details>

<details>
<summary><strong>制限、タイムアウト、長時間の作業</strong></summary>

- 1 コマンドの出力は最大 **1 MiB**、`exec_many` やバッチの結果は最大 **8 MiB** です。`output_truncated` を確認してください。
- `tool_timeout_sec` は上限にすぎず、各呼び出しはそれより短い独自のタイムアウトを持ちます。
- リモートコマンドは 2 回実行して安全とは限らないため、自動で再試行しません。
- Codex の呼び出しをキャンセルするとチャネルを閉じて `CANCELLED` を返します。開始済みのコマンドは動き続ける場合があります。
- 数分を超える作業はリモートホストの `tmux` / `screen` で行ってください。[`long-running.md`](../../skill/codex-hosts/references/long-running.md) を参照。

`probe` と `batch_probe` は読み取り専用ですが認証は行うため、セキュリティキーのタッチが必要な場合があります。Codex 側でも確認させるには次のようにします。

```toml
[mcp_servers.codex-hosts.tools.probe]
approval_mode = "prompt"

[mcp_servers.codex-hosts.tools.batch_probe]
approval_mode = "prompt"
```

</details>

<details>
<summary><strong>FIDO / セキュリティキー、SSH Agent、踏み台</strong></summary>

- **FIDO：** 既存の OpenSSH ECDSA-SK / Ed25519-SK Handle（`id_ecdsa_sk`、`id_ed25519_sk`）を Agent なしで使えます。秘密鍵はデバイスから出ず、PIN やタッチが必要な場合があります。アプリで作成する資格情報は ECDSA-SK です。
- **SSH Agent：** 起動済みの Windows OpenSSH Agent または Pageant に認証を委ねます。`codex-hosts` が Agent を起動・設定することはありません。
- **踏み台：** SSH ホストは保存済みで検証済みの他の SSH ホストを経由でき、チェーンは最大 8 台です。各 Hop でホスト鍵を検証し、ループは拒否され、Agent 転送は常に無効です。

</details>

## License

本プロジェクトは [Apache License 2.0](../../LICENSE) の下で提供されます。
