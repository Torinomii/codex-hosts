# codex-hosts

<p align="center">
  <a href="https://github.com/Torinomii/codex-hosts/actions/workflows/release.yml"><img src="https://github.com/Torinomii/codex-hosts/actions/workflows/release.yml/badge.svg" alt="建置 Release" /></a>
  <a href="../../LICENSE"><img src="https://img.shields.io/badge/License-Apache--2.0-D22128.svg" alt="Apache 2.0 授權條款" /></a>
  <img src="https://img.shields.io/badge/Windows-10%2B-0078D4?logo=windows&logoColor=white" alt="Windows 10 或更新版本" />
  <img src="https://img.shields.io/badge/Rust-1.92%2B-000000?logo=rust&logoColor=white" alt="Rust 1.92 或更新版本" />
  <a href="https://linux.do/"><img src="https://img.shields.io/badge/LINUX-DO-FFB003" alt="LINUX DO 社群" /></a>
</p>

[English](../../README.md) | [简体中文](README_zh-CN.md) | [繁體中文](README_zh-TW.md) | [日本語](README_ja.md)

`codex-hosts` 是供 Codex 使用的 Windows SSH / Telnet 主機管理工具，讓 Codex 可以連線並操作遠端主機，而不必在對話、命令參數或要求檔案中直接處理密碼、私鑰密碼片語和 FIDO PIN 等敏感憑據。

![Codex Hosts 主視窗](../../Main.png)

## 主要功能

- 管理可重複使用的 SSH / Telnet 主機設定。
- 支援密碼與一般 OpenSSH 私鑰驗證。
- 支援 FIDO / YubiKey 等硬體安全金鑰。
- 支援已載入 Windows OpenSSH Agent 或 Pageant 的身分。
- SSH 主機指紋固定與使用者確認。
- 支援已驗證的 SSH Jump Host / 跳板鏈。
- 支援單一主機命令執行。
- 支援在同一條 SSH 連線上並行執行多條命令。
- 支援 SSH / Telnet 多主機批次探測與執行。
- 支援僅存在於記憶體中的臨時秘密，可用於 API Key、Token 等不適合直接交給 Codex 的敏感參數。
- 提供完整 Codex Skill，可由 Codex 自動完成主機查找、連線、驗證與命令執行。

## 安裝

### 直接下載

預先建置版本支援 64 位元 Windows 10 或更新版本，可從 [Releases](https://github.com/Torinomii/codex-hosts/releases/latest) 下載。

### 安裝 Codex Skill

完整 Skill 的安裝結構：

```text
%USERPROFILE%\.codex\skills\codex-hosts\
├── SKILL.md
├── bin\
│   └── codex-hosts.exe
├── agents\
└── references\
```

手動安裝：

1. 將 Release 中的 `codex-hosts.exe` 放到：

```text
%USERPROFILE%\.codex\skills\codex-hosts\bin\codex-hosts.exe
```

2. 將 Release 中完整的 `skill\codex-hosts` 內容複製到：

```text
%USERPROFILE%\.codex\skills\codex-hosts
```

不要只複製 `SKILL.md` 或執行檔，應保留完整的 Skill 目錄。

也可以直接請 Codex 安裝：

```text
從 https://github.com/Torinomii/codex-hosts/releases/latest 下載並安裝最新版 codex-hosts。
請自動找到目前環境的 Skill 安裝目錄，安裝完整的 Skill 與執行檔，並確認所有必要檔案都已就位。
```

### 從原始碼建置

需要 Rust 1.92 或更新版本以及 MSVC 工具鏈：

```powershell
git clone https://github.com/Torinomii/codex-hosts.git
cd codex-hosts
cargo build --locked --release
```

生成的執行檔位於：

```text
target\release\codex-hosts.exe
```

建置完成後，將它放入 Skill 的 `bin` 目錄即可。

## 快速上手

### 1. 新增主機

開啟 `codex-hosts.exe`，新增一台主機。

填寫：

- 別名
- 位址或 IP
- 連接埠
- 使用者名稱
- 協定
- 驗證方式

然後儲存。

### 2. 選擇驗證方式

| 驗證方式 | 說明 |
| --- | --- |
| Password | 使用密碼登入，密碼儲存在 Windows 認證管理員中 |
| OpenSSH Key | 使用一般 OpenSSH 私鑰檔案 |
| FIDO / 安全金鑰 | 使用 OpenSSH FIDO Handle，由硬體裝置完成簽署 |
| SSH Agent | 使用已載入 Windows OpenSSH Agent 或 Pageant 的身分 |

這些驗證方式中的敏感值不會透過 Codex 對話傳遞。

### 3. 讓 Codex 使用主機

儲存主機後，只需要告訴 Codex 主機別名和要執行的工作。

例如：

```text
連線到 example，執行 hostname。
```

或者：

```text
檢查 web-1 和 web-2 的磁碟使用情況。
```

也可以一次要求執行多個操作：

```text
連線到 server1，檢查 hostname、uptime 和磁碟空間。
```

Codex Skill 會負責：

- 查找主機設定
- 建立連線
- 驗證 SSH Host Key
- 完成身分驗證
- 執行命令
- 傳回結構化結果

一般使用時不需要手動撰寫 Tool JSON。

## 安全界線

### 登入憑據

密碼與私鑰檔案密碼片語會持久儲存在 Windows 認證管理員中。

這些敏感值不會：

- 寫入主機設定
- 放入 Tool JSON
- 作為命令列參數傳遞
- 傳回給 Codex

FIDO PIN 只用於目前操作，不會儲存。

### SSH Host Key

SSH 主機指紋必須由使用者明確確認。

首次連線到未知主機時，`codex-hosts` 會顯示實際偵測到的主機指紋。

只有使用者確認後才會儲存。

如果伺服器 Host Key 之後發生變更，程式不會自動取代已儲存的指紋，必須再次由使用者明確確認。

### 臨時秘密

`codex-hosts` 也可以暫時保存 API Key、Token 等與主機登入無關的敏感參數。

這些值只保存在目前 `codex-hosts` 處理程序的記憶體中，不會儲存到 Windows 認證管理員，也不會傳回給 Codex。

使用時可以在使用者核准後直接注入指定程式的環境變數。

退出 `codex-hosts`、登出或重新啟動系統後，這些臨時值會失效。

完整說明請見 [`temporary-secrets.md`](../../skill/codex-hosts/references/temporary-secrets.md)。

## FIDO / 安全金鑰

`codex-hosts` 可以使用現有的 OpenSSH ECDSA-SK 與 Ed25519-SK FIDO Handle。

例如：

```text
id_ecdsa_sk
id_ed25519_sk
```

透過應用程式建立或恢復新的 FIDO SSH 憑據時，會使用 ECDSA-SK。

FIDO 模式下：

- 硬體私鑰不會離開安全裝置。
- FIDO PIN 只用於目前操作，不會儲存。
- 不需要啟用 SSH Agent。
- Agent Forwarding 一律停用。
- 依照安全金鑰設定，驗證過程可能需要 PIN 或 Touch。

FIDO Handle 與 SSH Agent 是兩種不同的驗證方式。

FIDO Handle：

```text
codex-hosts
    │
    ▼
OpenSSH FIDO Handle
    │
    ▼
安全金鑰
```

由 `codex-hosts` 直接透過系統元件完成硬體簽署。

SSH Agent：

```text
codex-hosts
    │
    ▼
OpenSSH Agent / Pageant
    │
    ▼
已載入的身分
```

驗證工作交給已經執行中的 Agent。

`codex-hosts` 不會為了使用 Agent 模式而自動啟動、啟用或持久化 Agent 服務。

## Jump Host

SSH 主機可以使用其他已儲存並驗證的 SSH 主機作為 Jump Host。

例如：

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

安全規則：

- Jump Host 必須是已驗證的 SSH 主機。
- 每個 Hop 都需要驗證 Host Key。
- 不允許 Jump Host 鏈循環。
- 一條 SSH 鏈最多包含 8 台主機。
- Agent Forwarding 一律停用。

## Telnet

`codex-hosts` 同樣支援 Telnet。

但 Telnet 本身沒有加密，使用者名稱、密碼、命令和傳回資料都可能以明文形式在網路中傳輸。

因此只應在你明確接受該風險的可信網路中使用 Telnet。

面向網際網路的連線建議使用 SSH。

## 批次執行

### SSH 單主機多命令

`exec_many` 可以重複使用同一條 SSH 連線，同時執行多條獨立短命令：

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

它只進行一次 SSH 驗證，然後透過同一條連線建立多個 Channel。

對 FIDO / 安全金鑰特別有用，因為一組命令通常可以共用一次身分驗證。

`exec_many` 僅適用於 SSH 主機。

### 多主機批次執行

可以明確指定多台主機：

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

也可以批次測試連線：

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

批次模式要求明確指定主機清單。

空清單不會被解讀成「全部主機」。

<details>
<summary>Codex / Tool 介面</summary>

### GUI 編輯模式

可以從 Codex 或指令碼開啟主機編輯器，並預先填入非敏感連線資訊：

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

不要將密碼、私鑰密碼片語或 FIDO PIN 作為參數傳遞。

驗證參數使用固定名稱：

- `password`：密碼驗證。
- `private-key` / `private_key`：一般私鑰檔案或 FIDO Handle，也接受 `fido-handle`。
- `ssh-agent` / `ssh_agent`：Windows OpenSSH Agent 或 Pageant。

`private-key` 同時用於一般 OpenSSH 私鑰與 FIDO Handle。

`ssh-agent` 只表示 Agent / Pageant 模式，不代表所有硬體金鑰驗證方式。

### Tool 模式

Tool 模式透過 UTF-8 JSON 要求檔案與結果檔案通訊，兩者都不得包含憑據。

常見要求：

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

`agent_identities` 和 `fido_identities` 只會傳回公開的身分資訊與公開金鑰。

執行遠端命令時，請檢查結果中的 `output_truncated`，確認輸出是否因長度限制而被截斷。

完整的 Codex 行為、呼叫流程與安全規則請見 [`SKILL.md`](../../skill/codex-hosts/SKILL.md)。

</details>

## 主機備註、標籤與命令輸入

主機支援多行 `description` 備註與多個 `tags` 標籤。編輯器可新增、移除標籤；標籤會去除前後空白、空項及不分大小寫的重複項，保留首次輸入的寫法。僅修改備註或標籤不會清除連線驗證與主機金鑰信任。搜尋涵蓋別名、位址、使用者名稱、備註及標籤；多個標籤篩選條件必須全部符合。批次全選只選取可見主機，切換篩選會移除隱藏主機的選取。

`list_hosts` 傳回這兩個欄位，並接受選用的 `tags` 陣列。省略或空陣列會傳回全部主機；不存在的標籤會傳回空清單。編輯器預填支援 `--description "備註"` 及重複的 `--tag prod --tag web`。省略欄位保留原值；空備註或單個空標籤可清空對應欄位。CSV 範本及匯入匯出新增選用的 `description`、`tags` 欄；標籤儲存格使用 `["prod","web"]` 這類 JSON 陣列，依 CSV 規則引用。舊設定與 CSV 仍可使用。

```json
{"action":"list_hosts","tags":["prod","web"]}
{"action":"exec","alias":"example","command":"python3 -","stdin":"print('hello')\n","command_timeout_ms":10000}
```

單主機 SSH `exec` 支援選用的 UTF-8 `stdin`，上限為 1 MiB。用戶端原樣傳送，不自行加入換行，完成後傳送 EOF。省略或 `null` 保留原有行為；`""` 立即傳送 EOF。輸入傳送與輸出讀取同時進行，沿用原有逾時及輸出限制。遠端程式可在讀完輸入前結束，此時以遠端結束狀態為準。

`capabilities` 傳回 `exec_stdin`、`exec_stdin_protocols`、`max_stdin_bytes`、`host_metadata_fields` 及 `list_hosts_tag_filter`。輸入過大傳回 `STDIN_TOO_LARGE`；對 Telnet、`exec_many` 或 `batch_exec` 提供 stdin 會傳回 `STDIN_UNSUPPORTED`。程式不會自動重播命令。

## 執行限制

為了避免異常命令產生無限輸出或占用過多記憶體，命令執行有明確限制。

- 單條命令最多擷取 **1 MiB** 輸出。
- 完整 `exec_many` 或批次 JSON 結果最大為 **8 MiB**。
- `exec_many` 與批次工作使用受限並行。
- 輸出被截斷時，可透過 `output_truncated` 判斷。
- 遠端命令不會因網路錯誤而自動重新執行。

不自動重試是因為遠端命令可能不是冪等操作，例如：

```text
reboot
rm
systemctl restart
資料庫寫入
部署操作
```

網路錯誤並不代表再次執行相同命令一定安全。

## 專案結構

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

本專案使用 [Apache License 2.0](../../LICENSE)。
