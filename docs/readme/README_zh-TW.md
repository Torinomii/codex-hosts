<h1 align="center">codex-hosts</h1>

<p align="center">儲存主機，掌握憑據，讓 Codex 完成遠端任務。</p>

<p align="center">
  <a href="https://github.com/Torinomii/codex-hosts/tree/v0.3.3"><img src="https://img.shields.io/badge/version-0.3.3-2563eb" alt="v0.3.3" /></a>
  <a href="https://github.com/Torinomii/codex-hosts/actions/workflows/release.yml"><img src="https://github.com/Torinomii/codex-hosts/actions/workflows/release.yml/badge.svg" alt="建置 Release" /></a>
  <a href="../../LICENSE"><img src="https://img.shields.io/badge/License-Apache--2.0-D22128.svg" alt="Apache 2.0 授權條款" /></a>
  <img src="https://img.shields.io/badge/Windows-10%2B-0078D4?logo=windows&logoColor=white" alt="Windows 10 或更新版本" />
  <a href="https://linux.do/"><img src="https://img.shields.io/badge/LINUX-DO-FFB003" alt="LINUX DO 社群" /></a>
</p>

<p align="center">
  <a href="../../README.md">English</a> · <a href="README_zh-CN.md">简体中文</a> · <a href="README_zh-TW.md">繁體中文</a> · <a href="README_ja.md">日本語</a>
</p>

<p align="center">
  <a href="https://github.com/Torinomii/codex-hosts/releases">下載</a> · <a href="#安裝">安裝</a> · <a href="#快速上手">快速上手</a> · <a href="#版本記錄">版本記錄</a> · <a href="#技術參考">技術參考</a>
</p>

供 Codex 使用的 Windows SSH / Telnet 主機管理工具與 MCP 伺服器。儲存主機後，就能讓 Codex 連線、執行命令或批次操作多台機器。密碼、私鑰密碼片語和 FIDO PIN 在本機輸入，不進入對話或工具參數。

<p align="center">
  <img src="../../Main.png" alt="Codex Hosts 主視窗" width="960" />
</p>

## 功能概覽

| 能力 | 可以做什麼 |
| --- | --- |
| 主機管理 | 儲存 SSH / Telnet 設定、備註與標籤，支援搜尋、篩選、匯入及匯出。 |
| 多種驗證 | 支援密碼、OpenSSH 私鑰、FIDO / YubiKey、Windows OpenSSH Agent 與 Pageant。 |
| 遠端命令 | 執行命令、透過 stdin 傳送文字，或用 sudo / su 明確指定執行使用者。 |
| 並行與批次 | 在同一條 SSH 連線上執行多條命令，或對明確指定的主機清單進行探測及執行。 |
| 連線控制 | 驗證主機指紋與跳板鏈，依主機選擇逐次驗證、工作階段保持或閒置後斷開。 |
| 呼叫審批 | 查看 SSH 完整命令與等待批次，為臨時秘密的每個欄位獨立設定審批。 |
| 臨時秘密 | 將 API Key、Token 保存在行程記憶體中，注入指定本機程式的環境變數。 |

## 0.3.3 更新亮點

- **sudo / su：** 每次指定執行使用者、絕對路徑程式與原樣參數；儲存密碼供給分別授權，關閉時仍可免密執行。
- **SSH 呼叫審批：** 每次呼叫前查看完整命令、輸入與批次，保持連線也不會重用審批。
- **臨時秘密：** 每個欄位預設開啟審批，儲存或清除欄位會撤銷等待中的執行。
- **編輯器與憑據：** 選用帳號密碼預設摺疊並依帳號綁定，切換登入帳號不會轉移或洩露另一帳號的儲存密碼。
- **引導安裝：** 完整 Skill 統一使用符號連結，支援環境檢查、替換確認與備份。

[查看版本差異](https://github.com/Torinomii/codex-hosts/compare/v0.3.2...v0.3.3)

## 安裝

使用預先編譯套件可以最快完成安裝，也可從固定路徑的原始碼目錄建置。兩種方式都透過符號連結安裝完整 Skill。

| 方式 | 所需環境 | 入口 |
| --- | --- | --- |
| GitHub Release | 64 位元 Windows 10+、PowerShell 7.2+ | `install.cmd` 或下方命令 |
| 原始碼建置 | 上述環境，加 Rust 1.92+、MSVC 與 Windows SDK | `scripts/install.cmd` 或 `scripts/install-local-skill.ps1` |

### 1. 安裝程式與 Skill

從 [Releases 頁面](https://github.com/Torinomii/codex-hosts/releases)下載 `codex-hosts-windows-x86_64.zip`，解壓到固定目錄，然後在該目錄執行：

```powershell
pwsh -NoProfile -File .\install-local-skill.ps1 -ReleasePackage
```

ZIP 預設包含程式、完整 Skill、`install-local-skill.ps1` 與 `install.cmd`。按兩下 `install.cmd` 可進入相同的分步流程，缺少 PowerShell 7.2+ 時會提供安裝提示。Release 安裝不需要 Rust 或 Visual Studio；環境檢查後確認安裝路徑，發現舊副本、過期連結或錯誤的程式路徑時，再確認更新覆寫。

0.3.1 之後的 Release 另附 `codex-hosts-windows-x86_64.zip.sha256`，解壓前請與 `(Get-FileHash .\codex-hosts-windows-x86_64.zip).Hash` 核對。

**請保留解壓目錄或原始碼目錄。** 已安裝的 Skill 會連結到它，目前 Windows 使用者須有建立符號連結的權限。安裝時保留完整套件，包括參考文件與執行檔。

### 2. 註冊 MCP 伺服器

在 `%USERPROFILE%\.codex\config.toml`（或工作區的 `.codex\config.toml`）中加入一次，將 `<user>` 替換為 Windows 帳號名稱：

```toml
[mcp_servers.codex-hosts]
command = "C:\\Users\\<user>\\.codex\\skills\\codex-hosts\\bin\\codex-hosts.exe"
args = ["--mcp"]
startup_timeout_sec = 20
tool_timeout_sec = 86400
```

<details>
<summary><strong>從原始碼建置，或連結已有原始碼目錄</strong></summary>

### 從原始碼建置

```powershell
pwsh -NoProfile -File .\scripts\install-local-skill.ps1
```

建議執行 `pwsh -NoProfile -File .\scripts\install-local-skill.ps1`（也可按兩下 `scripts\install.cmd`）。指令碼自動檢查 PowerShell 7.2+、Rust 1.92+、x86_64 MSVC 工具鏈與 Windows SDK，再依序詢問是否開始編譯、是否安裝軟連結。缺少工具時提供指引，不會自動安裝。引導式編譯輸出為 `target\x86_64-pc-windows-msvc\release\codex-hosts.exe`。

也可使用 Rust 1.92+ 與 MSVC 工具鏈手動編譯：

```powershell
git clone https://github.com/Torinomii/codex-hosts.git
cd codex-hosts
cargo build --locked --release
```

生成的執行檔位於：

```text
target\release\codex-hosts.exe
```

手動建置後，執行下方的連結安裝命令並加上 `-SkipBuild`；指令碼優先使用已有的引導式編譯產物，否則使用 `target\release\codex-hosts.exe`。

### 連結本機原始碼目錄

若在固定路徑的本機原始碼目錄中開發，可安裝軟連結，之後每次建置都不必再複製：

```powershell
pwsh -NoProfile -File .\scripts\install-local-skill.ps1 -SkipBuild
```

指令碼將已安裝的 `codex-hosts` 目錄整體連結到 `skill\codex-hosts`，其中的 `bin\codex-hosts.exe` 再連結到所選建置產物；重新建置相同目標後無須複製。發現複製式檔案/目錄、錯誤或懸空連結時，會顯示舊、新路徑並詢問是否替換。原內容保留在相鄰的 `.codex-hosts.backup-*` 路徑中；安裝失敗時嘗試還原兩處連結。正確連結保持不變。請保留原始碼或解壓目錄；移動後重新執行指令碼並確認新目標。

Windows 必須允許建立符號連結。檢查失敗時，請自行開啟「開發人員模式」或以系統管理員身分執行 PowerShell 後重試。指令碼不會修改系統設定、關閉應用程式或退回複製安裝；若執行中的程式被鎖定，請在方便時自行關閉後再編譯。

</details>

<details>
<summary><strong>安裝參數與 Skill 目錄結構</strong></summary>

安裝後的 Skill 結構：

```text
%USERPROFILE%\.codex\skills\codex-hosts\
├── SKILL.md
├── bin\
│   └── codex-hosts.exe
├── agents\
└── references\
```

可用 `-CodexHome <目錄>` 覆寫 `CODEX_HOME` / `%USERPROFILE%\.codex`（例如 `-CodexHome "$env:USERPROFILE\.agents"` 安裝到 `.agents\skills`）。自動化可用 `-NonInteractive -Build -Install`，已有建置產物則用 `-NonInteractive -SkipBuild -Install`；替換舊內容還須明確加上 `-ReplaceExisting`。`-SkipInstall` 表示只編譯不安裝，未明確同意的提示預設選擇否。

從本機製作的發佈 ZIP 安裝時也使用相同指令碼，加上 `-ReleasePackage`，並保留解壓目錄。

也可以直接讓 Codex 安裝：

```text
將 https://github.com/Torinomii/codex-hosts.git 複製到固定目錄，執行 scripts/install-local-skill.ps1 檢查編譯環境；編譯、安裝軟連結與替換舊路徑前都先讓我確認。
確認已安裝的整個 codex-hosts Skill 目錄是單一符號連結、執行檔解析至發行版建置，依 SKILL.md 在 config.toml 中註冊 codex-hosts MCP 伺服器，並保留原始碼目錄。
```

</details>

## 快速上手

### 1. 新增主機

開啟 `codex-hosts.exe`，新增主機，填寫別名、位址、連接埠、使用者名稱、協定與驗證方式。按需加入備註與標籤，再儲存。

### 2. 選擇驗證方式

| 驗證方式 | 說明 |
| --- | --- |
| Password | 使用密碼登入，密碼儲存在 Windows 認證管理員中 |
| OpenSSH Key | 使用一般 OpenSSH 私鑰檔案 |
| FIDO / 安全金鑰 | 使用 OpenSSH FIDO Handle，由硬體裝置完成簽署 |
| SSH Agent | 使用已載入 Windows OpenSSH Agent 或 Pageant 的身分 |

這些驗證方式中的敏感值不會透過 Codex 對話傳遞。

### 3. 讓 Codex 使用主機

告訴 Codex 已儲存的主機別名與任務即可，例如：

```text
連線到 example，執行 hostname。
檢查 web-1 和 web-2 的磁碟使用情況。
連線到 server1，檢查 hostname、uptime 和磁碟空間。
```

Skill 會查找主機、驗證指紋、完成認證並回傳結構化執行結果，日常使用不必手動呼叫 MCP 工具。

## 安全界線

| 控制項 | 行為 |
| --- | --- |
| 登入憑據 | 密碼與私鑰密碼片語儲存在 Windows 認證管理員；FIDO PIN 不儲存。 |
| 主機身分 | 未知或變更的 SSH 指紋需要確認，跳板鏈中的每一跳也會驗證。 |
| 工作階段保持 | 重用工作階段會略過後續認證與硬體金鑰觸碰；呼叫審批是獨立設定。 |
| sudo / su | 每次呼叫明確選擇使用者；儲存密碼供給分別授權，只處理最外層認證。 |
| 臨時秘密 | 只保存在記憶體中，每個欄位預設要求審批；退出系統匣程式後清空。 |

> **Telnet 不提供加密。** 憑據、命令與結果可能以明文傳輸。僅在接受此風險的可信網路中使用，網際網路連線優先選擇 SSH。

<details>
<summary><strong>憑據儲存、主機信任、認證保持與臨時秘密的詳細說明</strong></summary>

### 登入憑據

密碼與私鑰檔案密碼片語會持久儲存在 Windows 認證管理員中。

這些敏感值不會：

- 寫入主機設定
- 放入 MCP 工具參數或結果
- 作為命令列參數傳遞
- 傳回給 Codex

FIDO PIN 只用於目前操作，不會儲存。

### SSH Host Key

SSH 主機指紋必須由使用者明確確認。

首次連線到未知主機時，`codex-hosts` 會顯示實際偵測到的主機指紋。

只有使用者確認後才會儲存。

如果伺服器 Host Key 之後發生變更，程式不會自動取代已儲存的指紋，必須再次由使用者明確確認。

### 認證保持

舊版本的主機和匯入的主機在 Codex 的每次呼叫都重新認證：硬體金鑰每個動作觸碰一次，Codex 在兩次呼叫之間不持有任何開啟的工作階段。新建主機預選「Codex 工作階段期間保持」；編輯器會在首次儲存時以及之後每次變更（無論改向哪一檔）時要求確認。每台主機都可以在「驗證」卡片中選擇任意一項：

| 選項 | 行為 |
| --- | --- |
| 每次操作重新認證 | 呼叫結束即斷開連線。舊版本主機與匯入的主機從這一項開始。 |
| Codex 工作階段期間保持 | 新建主機預選。已認證的工作階段保持開啟並傳送 keepalive，直到 Codex 退出、連線中斷或呼叫 `disconnect`。 |
| 閒置後斷開 | 工作階段保持到閒置達設定的分鐘數為止。 |

保持工作階段期間，Codex 對該主機的後續命令不再重新認證或觸碰安全金鑰，也就取消了「每個動作一次確認」的保護。編輯器會顯示這則警告，保持工作階段的主機在清單中帶有標記，且 Skill 禁止 Codex 建議修改此設定。Telnet 主機一律每次重新認證。憑據、PIN 與主機金鑰檢查皆不受影響，被重用的只是活動中的工作階段。

### 臨時秘密

`codex-hosts` 也可以暫時保存 API Key、Token 等與主機登入無關的敏感參數。

這些值只保存在目前 `codex-hosts` 處理程序的記憶體中，不會儲存到 Windows 認證管理員，也不會傳回給 Codex。

每個欄位的「呼叫審批」使用與主介面一致的核取方塊，預設開啟。欄位名稱、遮罩秘密值和審批控制項依統一欄寬對齊，窄視窗自動上下排列。程式使用的欄位只要有一個開啟審批，就會在臨時秘密視窗內顯示藍色授權對話框，頂部按鈕只決定本次執行。點擊命令可開啟完整詳情，長文字支援分頁和複製全文；詳情裡的返回或 Esc 不作審批決定。所有引用欄位都關閉審批時，可直接注入指定程式的環境變數並執行。

頂部「全部刪除」經確認後移除所有欄位、已儲存值和未儲存草稿，同時取消等待中的操作；不會停止已執行的程式。單項「清空」和 `temporary_secrets_clear` 仍只清空值、保留欄位。

退出 `codex-hosts` 系統匣程式、登出或重新啟動系統後，這些臨時值會失效；Codex 的啟動或退出不影響它們。

完整說明請見 [`temporary-secrets.md`](../../skill/codex-hosts/references/temporary-secrets.md)。

</details>

## 版本記錄

展開版本可查看完整變更。目前 0.3.3 的亮點見上方。

<details>
<summary><strong>0.3.3 · sudo / su、呼叫審批與引導安裝</strong></summary>

- **Sudo / su：** `exec_sudo` 與 `exec_su` 以明確選擇的使用者執行絕對路徑程式及原樣參數。「進階」中的獨立開關只授權使用儲存密碼，關閉時仍可免密執行。密碼保持與帳號綁定，不進入工具參數。
- **編輯器：** sudo/su 使用與其他進階選項一致的表單列和核取方塊，選用帳號密碼預設摺疊；不重複登入輸入，不設定預設執行目標，也不再提供專用的 sudo/su 儲存測試按鈕。修改帳號不會把已有密碼轉移給另一使用者。
- **呼叫審批：** 依主機設定的選用開關在每次 SSH 呼叫前要求確認，涵蓋探測、使用者切換與批次操作。藍色對話框提供完整命令、輸入、詳情、到期提示與決定回饋；批次按鈕只作用於目前展示的呼叫批次，保持連線不會重用審批。
- **臨時秘密：** 每個欄位都有預設開啟的審批開關。引用欄位只要有一個要求審批，本次執行就需確認；全部關閉時直接執行，或等待目前本機子行程結束。儲存或清除欄位會撤銷等待中的執行。
- **Skill：** 更新 sudo/su 與授權對話框指引，說明免密路徑、明確的執行使用者與失敗處理。一般命令及腳本內部的 sudo/su 不會獲得透明自動供密。
- **憑據安全：** 修改登入帳號時輪換密碼記錄，進行中的 sudo/su 呼叫無法讀取另一帳號的新密碼。舊版密碼移轉成功後清理舊槽，刪除密碼時也會清理移轉來源。
- **安裝：** 原始碼與 Release 統一使用軟連結。分步指令碼檢查環境、詢問是否編譯與安裝；發現舊副本或錯誤連結時，提示更新路徑並保留備份。

[查看版本差異](https://github.com/Torinomii/codex-hosts/compare/v0.3.2...v0.3.3)

</details>

<details>
<summary><strong>0.3.2 · 完整 Skill 目錄連結</strong></summary>

- **整體連結：** 已安裝的 `codex-hosts` Skill 改為連結整個原始碼 Skill 目錄，使 `SKILL.md`、`agents` 與 `references` 保持在一起；執行檔在原始碼 Skill 內單獨連結。
- **Windows 建置準備：** 加入 `winresource` 建置依賴，套件版本更新為 0.3.2。

[查看版本差異](https://github.com/Torinomii/codex-hosts/compare/v0.3.1...v0.3.2)

</details>

<details>
<summary><strong>0.3.1 · MCP 工具、認證保持與連線可靠性</strong></summary>

- **MCP 伺服器：** `codex-hosts.exe --mcp` 透過 stdio 為 Codex 提供服務。主機、探測、命令、批次、主機編輯器與臨時秘密都是帶風險註解的 MCP 工具，不再經過 shell、PowerShell 包裝或臨時要求檔案。原有的檔案式 Tool 模式原樣保留，尚未更新的 Skill 仍可正常運作。
- **認證保持：** 每台主機現在都要選擇 Codex 是在 Codex 工作階段期間保持工作階段（新建主機預選；首次儲存和每次變更都會要求確認）、每次操作重新認證（原有行為，舊版本主機與匯入的主機保持此項），還是閒置指定分鐘後斷開。保持工作階段的主機在清單中帶有標記，編輯器中顯示警告。
- **編輯器：** 必填項標有 `*`，儲存時留空會被高亮；新增「進階」卡片，提供依主機的並行通道數、預設逾時、Keepalive 間隔、供 Codex 參考的遠端環境提示、對 Codex 隱藏主機以及 Telnet 提示字元覆寫。
- 取消 Codex 呼叫會立即停止等待並關閉通道；長時間作業改為依賴遠端主機自己的 `tmux` / `screen`，而不是長時間等待。
- `codex-hosts.exe --version` 輸出版本號，參數錯誤會寫到 stderr。
- **連線可靠性：** 改善真實 Telnet 登入提示與回顯處理、SSH 跳板連線清理、主機設定變更處理，以及保持工作階段的閒置計時。
- **診斷回饋：** MCP 結果附帶版本號，單一憑據讀取失敗不會導致整個主機清單無法使用；MCP 啟動時檢查主機設定檔的擁有者。
- **發佈交付：** Actions 驗證並打包程式、完整 Skill、連結安裝入口與 SHA-256 校驗檔案。

[查看版本差異](https://github.com/Torinomii/codex-hosts/compare/v0.3.0...v0.3.1)

</details>

<details>
<summary><strong>0.3.0 · 主機介面重新設計、備註與標籤</strong></summary>

- **主機備註與標籤：** 可依備註或標籤搜尋、篩選列表，Codex 也能透過 `--description`、`--tag` 預填；`list_hosts` 會回傳這兩個欄位並支援 `tags` 篩選，`exec` 新增 `stdin` 用於傳入命令輸入。
- **介面重新設計：** 主機列表可拖曳調整寬度、標頭更精簡；主機詳情依「基本資訊 / 連線 / 身分驗證 / SSH 主機金鑰」分區顯示；「測試連線」與「儲存」固定在底部操作列。
- **狀態列與通知：** 所有操作結果顯示在底部狀態列，警告與錯誤還會在右上角彈出可關閉的提示。
- **批次管理：** 工具列下方出現選取列，顯示已選數量以及全選、匯出、刪除；列表列附帶核取方塊，按 `Esc` 離開。
- 匯入與匯出改為對話框，需要關閉後才能繼續操作；所有確認對話框統一樣式，破壞性操作會醒目顯示。
- **版面隨視窗大小自適應：** 視窗寬度 1040 px 起表單為兩欄，最小視窗下標籤改為上下排列；同時支援 Windows 淺色與深色主題。

[查看版本差異](https://github.com/Torinomii/codex-hosts/compare/v0.2.3...v0.3.0)

</details>

## 技術參考

<details>
<summary><strong>MCP 工具</strong></summary>

Codex 透過 stdio 與 `codex-hosts.exe --mcp` 通訊。每個工具都同時以 `structuredContent` 與文字回傳同一份 JSON，並帶有 MCP 註解，方便策略層區分唯讀工具與會執行程式碼的工具。

| 工具 | 用途 |
| --- | --- |
| `list_hosts` | 已儲存主機的非敏感資訊、信任狀態、`auth_persistence`、`require_call_approval`、sudo/su 供密權限、`max_channels`、`remote_env` |
| `agent_identities`、`fido_identities` | 已載入 Agent 身分與 FIDO Handle 的公開資訊 |
| `probe`、`batch_probe` | 認證並執行 `hostname`；揭露未知或變更的主機金鑰 |
| `exec`、`exec_stdin`、`exec_many`、`batch_exec` | 執行命令；`exec_stdin` 單獨成工具，因為 stdin 中的程式內容對審查不透明 |
| `exec_sudo` | 在 SSH 主機上以明確指定的 `user` 執行絕對路徑程式及 argv；供密獲准時，一般 sudo 沿用目前使用者的 SSH 登入密碼 |
| `exec_su` | 透過 su 以明確指定的目標 `user` 執行絕對路徑程式；遠端原則允許時無需密碼 |
| `disconnect` | 斷開為已選擇保持工作階段的主機保留的連線 |
| `open_host_editor` | 開啟預填非敏感資訊的編輯器視窗，或確認回報的主機金鑰 |
| `temporary_secrets_open`、`_status`、`_run`、`_clear` | 由系統匣程式持有的僅記憶體秘密 |

參數中永不包含密碼、密碼片語或 PIN；編輯器在遮罩輸入框中收集它們。逾時（`connect_timeout_ms`、`command_timeout_ms`、`batch_timeout_ms`）按呼叫傳入，省略時預設連線 120 秒、單一命令 10 分鐘，或取主機「進階」卡片中的預設值。

SSH 主機的「進階」中有「每次 Codex 呼叫前審批」，預設關閉。目標或跳板主機開啟後，連線前以藍色對話框審批本次呼叫涉及的全部主機與命令。授權涵蓋這次 Codex 呼叫從啟動到結束的生命週期；下次呼叫須重新審批，保持連線不會延長授權。頂部「全部允許 / 全部拒絕」只處理目前展示的這批等待呼叫，後來到達的請求須點擊「查看下一批」。點擊命令可開啟完整詳情，區分實際執行主機和跳板，並展示完整命令、標準輸入及 sudo 使用者；詳情頂部按鈕決定整次呼叫。長文字可分頁查看和複製全文，詳情中的返回或 Esc 不作決定。介面顯示數量、到期倒數及審批回饋；允許按鈕為紅色，拒絕按鈕為白色。

SSH 呼叫審批、臨時秘密審批與 sudo/su 供密權限彼此獨立。等待審批的要求在五分鐘後到期，審批通過不代表命令執行成功。對話框行為與失敗處理見 [approvals.md](../../skill/codex-hosts/references/approvals.md)。

「進階」中的「允許 sudo 使用儲存的密碼」僅授權使用儲存的密碼。關閉時仍可使用免密或既有認證快取；實際要求密碼時回傳 `SUDO_AUTO_AUTH_DISABLED`，不為該認證請求讀取或傳送密碼。執行使用者由 Codex 每次選擇：`exec_sudo` 必填 `alias`、絕對路徑 `program`、`user`，可選原樣 `args` 和逾時參數；需要 root 時明確傳入 `"user":"root"`，不再儲存預設目標。sudo/su 開關採用與其他選項一致的表單列和核取方塊。密碼登入直接沿用 SSH 登入密碼，不重複顯示登入使用者名稱和密碼框；金鑰、Agent 登入或已有獨立密碼的帳號，可在開啟供密後展開「獨立系統密碼（選填）」。額外帳號在「其他帳號密碼（選填）」摺疊區管理。不使用其他使用者的登入密碼、金鑰口令或 Agent 憑據；`rootpw`、`targetpw`、`runaspw` 依實際認證提示選擇密碼。空白保留已有密碼；移除獨立密碼後仍可沿用符合條件的登入密碼。改名刪除舊密碼，可輸入替換密碼或留空。不再提供專用 sudo/su 儲存測試按鈕，一般連線測試保持不變。不接受原始命令、密碼或業務 stdin，不自動重試。shell 啟動器及 TTY/PAM 限制見 [sudo.md](../../skill/codex-hosts/references/sudo.md)。

「進階」中的「允許 su 使用儲存的密碼」獨立授權 su 使用儲存的密碼；關閉時仍可免密執行，實際要求密碼時回傳 `SU_AUTO_AUTH_DISABLED`。`exec_su` 同樣要求每次填寫 `user`、絕對路徑 `program`，可選原樣 `args` 和逾時參數。目標使用者的選用密碼在「其他帳號密碼」摺疊區每使用者一列管理；su 供密授權允許時可沿用同一帳號密碼，無需重複輸入登入密碼。遠端允許的 root 到一般使用者等免密路徑無需記錄，完全不讀取或傳送密碼。遠端須提供 util-linux `/usr/bin/su`、`/usr/bin/env`、`/bin/sh` 及標準 Linux-PAM 提示；無 PTY 或業務 stdin。舊 JSON 預設目標會被忽略，已存密碼槽保留；舊呼叫省略 `user` 會失敗且不執行。詳見 [su.md](../../skill/codex-hosts/references/su.md)。

這兩個工具只處理外層 sudo/su 認證。一般 `exec` / `exec_stdin` 和已啟動腳本內部的 sudo/su 不會自動供密；內部失敗不會觸發整段腳本提權或自動重播。

`open_host_editor` 的驗證參數使用穩定名稱：`password`；`private-key` / `private_key` 表示一般私鑰檔案或 FIDO Handle（也接受 `fido-handle`）；`ssh-agent` / `ssh_agent` 表示 Windows OpenSSH Agent 或 Pageant。

執行遠端命令時請檢查 `output_truncated`，判斷輸出是否因大小限制被截斷。

完整的 Codex 行為、工作流程與安全規則見 [`SKILL.md`](../../skill/codex-hosts/SKILL.md)。

</details>

<details>
<summary><strong>命令並行與多主機範例</strong></summary>

### SSH 單主機多命令

`exec_many` 可以重複使用同一條 SSH 連線，同時執行多條獨立短命令：

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

它只進行一次 SSH 驗證，然後透過同一條連線建立多個 Channel。

對 FIDO / 安全金鑰特別有用，因為一組命令通常可以共用一次身分驗證。

`exec_many` 僅適用於 SSH 主機。

### 多主機批次執行

可以明確指定多台主機：

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

也可以批次測試連線：

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

批次模式要求明確指定主機清單。

空清單不會被解讀成「全部主機」。

</details>

<details>
<summary><strong>備註、標籤、CSV 與 stdin</strong></summary>

主機支援多行 `description` 備註與多個 `tags` 標籤。編輯器可新增、移除標籤；標籤會去除前後空白、空項及不分大小寫的重複項，保留首次輸入的寫法。僅修改備註或標籤不會清除連線驗證與主機金鑰信任。搜尋涵蓋別名、位址、使用者名稱、備註及標籤；多個標籤篩選條件必須全部符合。批次全選只選取可見主機，切換篩選會移除隱藏主機的選取。

`list_hosts` 傳回這兩個欄位，並接受選用的 `tags` 陣列。省略或空陣列會傳回全部主機；不存在的標籤會傳回空清單。`open_host_editor` 可預填 `description` 與 `tags`。省略欄位保留原值；空備註或單個空標籤可清空對應欄位。CSV 範本及匯入匯出新增選用的 `description`、`tags` 欄；標籤儲存格使用 `["prod","web"]` 這類 JSON 陣列，依 CSV 規則引用。舊設定與 CSV 仍可使用。

```json
{"tags":["prod","web"]}
{"alias":"example","command":"python3 -","stdin":"print('hello')\n","command_timeout_ms":10000}
```

`exec_stdin` 向單台 SSH 主機的命令傳送精確的 UTF-8 文字，上限 1 MiB，不加入換行，完成後傳送 EOF；`""` 立即傳送 EOF。輸入傳送與輸出讀取同時進行，沿用原有逾時及輸出限制。遠端程式可在讀完輸入前結束，此時以遠端結束狀態為準。輸入過大傳回 `STDIN_TOO_LARGE`；Telnet 主機傳回 `STDIN_UNSUPPORTED`。程式不會自動重播命令。

</details>

<details>
<summary><strong>逾時、探測審批與長時間任務</strong></summary>

`tool_timeout_sec` 只是上限；每次呼叫都帶有自己的逾時，且必須低於該上限。若保留 Codex 的預設值（300 秒），超過它的命令仍會執行到自身逾時（未指定時為 10 分鐘），不保持工作階段的主機的後續呼叫要等它結束。

`probe` 與 `batch_probe` 標註為唯讀，只執行 `hostname`，但仍會連線及認證，硬體金鑰可能要求觸碰。主機的呼叫審批設定同樣適用於探測。如需額外要求 Codex 確認，可使用：

```toml
[mcp_servers.codex-hosts.tools.probe]
approval_mode = "prompt"

[mcp_servers.codex-hosts.tools.batch_probe]
approval_mode = "prompt"
```

為了避免異常命令產生無限輸出或占用過多記憶體，命令執行有明確限制。

- 單條命令最多擷取 **1 MiB** 輸出。
- 完整 `exec_many` 或批次 JSON 結果最大為 **8 MiB**。
- `exec_many` 與批次工作使用受限並行。
- 輸出被截斷時，可透過 `output_truncated` 判斷。
- 遠端命令不會因網路錯誤而自動重新執行。
- 取消 Codex 呼叫會關閉通道並傳回 `CANCELLED`；已經開始的遠端命令可能繼續執行。
- 超過幾分鐘的工作應放進遠端主機的 `tmux` / `screen`，見 [`long-running.md`](../../skill/codex-hosts/references/long-running.md)。

不自動重試是因為遠端命令可能不是冪等操作，例如：

```text
reboot
rm
systemctl restart
資料庫寫入
部署操作
```

網路錯誤並不代表再次執行相同命令一定安全。

</details>

<details>
<summary><strong>FIDO / 安全金鑰與 SSH Agent</strong></summary>

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

</details>

<details>
<summary><strong>SSH 跳板鏈</strong></summary>

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

</details>

<details>
<summary><strong>專案結構</strong></summary>

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

</details>

## License

本專案使用 [Apache License 2.0](../../LICENSE)。
