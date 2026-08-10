# codex-hosts

<p align="center">
  <a href="https://github.com/Torinomii/codex-hosts/actions/workflows/release.yml"><img src="https://github.com/Torinomii/codex-hosts/actions/workflows/release.yml/badge.svg" alt="建置 Release" /></a>
  <a href="../../LICENSE"><img src="https://img.shields.io/badge/License-Apache--2.0-D22128.svg" alt="Apache 2.0 授權條款" /></a>
  <img src="https://img.shields.io/badge/Windows-10%2B-0078D4?logo=windows&logoColor=white" alt="Windows 10 或更新版本" />
  <img src="https://img.shields.io/badge/Rust-1.92%2B-000000?logo=rust&logoColor=white" alt="Rust 1.92 或更新版本" />
  <a href="https://linux.do/"><img src="https://img.shields.io/badge/LINUX-DO-FFB003" alt="LINUX DO 社群" /></a>
</p>

[English](../../README.md) | [简体中文](README_zh-CN.md) | [繁體中文](README_zh-TW.md) | [日本語](README_ja.md)

`codex-hosts` 是供 Codex 使用的 Windows SSH / Telnet 主機管理工具，讓 Codex 不必接觸密碼或金鑰也能安全地發起連線。

![Codex Hosts 主視窗](../../Main.png)

## 主要功能

- 儲存伺服器資訊，供 Codex 連線時使用。
- 支援密碼、一般 OpenSSH 金鑰，以及 FIDO/YubiKey 等硬體金鑰登入。
- 支援已載入 Windows OpenSSH Agent 或 Pageant 的身分。

## 安裝

### 直接下載

預先建置的版本支援 64 位元 Windows 10 或更新版本，可從 [Releases](https://github.com/Torinomii/codex-hosts/releases/latest) 下載。

如果要手動安裝：

1. 將 `bin\codex-hosts.exe` 放到 `skill\codex-hosts\bin\codex-hosts.exe`。
2. 將完整的 `skill\codex-hosts` 資料夾複製到 `%USERPROFILE%\.codex\skills\codex-hosts`。

也可以直接請 Codex 安裝：

```text
從 https://github.com/Torinomii/codex-hosts/releases/latest 下載並安裝最新版 codex-hosts。請自動找到目前環境的 Skill 安裝目錄，安裝完整的 Skill 和執行檔，並確認所有必要檔案都已就位。
```

### 從原始碼建置

需要 Rust 1.92 或更新版本以及 MSVC 工具鏈：

```powershell
git clone https://github.com/Torinomii/codex-hosts.git
cd codex-hosts
cargo build --locked --release
```

建置完成後，將 `target\release\codex-hosts.exe` 複製到 `skill\codex-hosts\bin\codex-hosts.exe`，再安裝完整的 Skill 資料夾。

## 快速上手

1. 開啟 `codex-hosts.exe`，新增一台主機。
2. 填寫別名、位址、連接埠和使用者名稱，再選擇登入方式並儲存。

<details>
<summary>介面</summary>

### 從 Codex 或指令碼呼叫

GUI 編輯模式可以預填不敏感的連線資訊：

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

驗證參數使用固定名稱：

- `password`：密碼登入。
- `private-key` / `private_key`：金鑰檔案或 FIDO 控制代碼，也接受 `fido-handle`。
- `ssh-agent` / `ssh_agent`：正在執行的 SSH Agent 或 Pageant。

工具模式透過要求檔案和結果檔案運作，兩者都不能包含認證資訊：

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

`exec_many` 只登入一次，再透過同一條 SSH 連線同時執行多條短命令；使用硬體金鑰時，一組命令通常只需要驗證一次。`agent_identities` 和 `fido_identities` 只會傳回公開的身分資訊與公開金鑰。執行遠端命令時，請檢查結果中的 `output_truncated`，確認輸出是否因長度限制而被截斷。

</details>

## 安全界線

- 密碼和金鑰檔案密碼片語只會儲存在 Windows 認證管理員中；FIDO PIN 只用於目前操作，不會儲存。
- SSH 主機指紋必須由使用者明確確認，程式不會自動取代已儲存的指紋。
- FIDO 直接簽署不會啟動或啟用 Agent 服務；Agent 轉送也一律關閉。
- 只有已驗證的 SSH 主機才能作為跳板，跳板鏈最多包含八台主機，並會檢查循環。
- Telnet 會以明文傳輸帳號和資料，只應在明確接受風險的可信網路中使用。
- 單條命令最多接收 1 MiB 輸出；`exec_many` 或完整批次結果寫成 JSON 後最多為 8 MiB。程式會在接收輸出時直接套用預算，不會先在記憶體中保存多份大型結果。
