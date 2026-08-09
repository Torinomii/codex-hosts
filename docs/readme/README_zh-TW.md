# codex-hosts

[English](../../README.md) | [简体中文](README_zh-CN.md) | [繁體中文](README_zh-TW.md) | [日本語](README_ja.md)

<p align="center">
  <a href="https://github.com/Torinomii/codex-hosts/actions/workflows/release.yml"><img src="https://github.com/Torinomii/codex-hosts/actions/workflows/release.yml/badge.svg" alt="建置 Release" /></a>
  <a href="../../LICENSE"><img src="https://img.shields.io/badge/License-Apache--2.0-D22128.svg" alt="Apache 2.0 授權條款" /></a>
  <img src="https://img.shields.io/badge/Windows-10%2B-0078D4?logo=windows&logoColor=white" alt="Windows 10 或更新版本" />
  <img src="https://img.shields.io/badge/Rust-1.92%2B-000000?logo=rust&logoColor=white" alt="Rust 1.92 或更新版本" />
  <a href="https://linux.do/"><img src="https://img.shields.io/badge/LINUX-DO-FFB003" alt="LINUX DO 社群" /></a>
</p>

`codex-hosts` 是供 Codex 使用的 Windows SSH / Telnet 主機管理工具，讓 Codex 不必接觸密碼或金鑰即可安全地發起連線。

![Codex Hosts 主視窗](../../Main.png)

## 主要功能

- 儲存別名、位址、連接埠、使用者名稱、通訊協定、驗證方式、選填私密金鑰路徑、已信任的 SSH 主機金鑰和選填跳板主機。
- 支援 SSH 密碼與鍵盤互動驗證，以及含選填密碼片語的 OpenSSH 私密金鑰。
- 密碼與私密金鑰密碼片語僅儲存在 Windows 認證管理員中。
- 使用已儲存且驗證成功的 SSH 主機建立多層跳板鏈。
- 可用於已明確接受風險的傳統 Telnet 環境；Telnet 認證與流量都不會加密。
- Codex 可以預先建立指定別名的草稿並等候儲存、信任或取消，不必在對話中詢問認證。

## 安裝

### 下載 Release

預先建置的套件支援 64 位元 Windows 10 或更新版本，可從 [Releases](https://github.com/Torinomii/codex-hosts/releases/latest) 下載。

### 安裝

將 `bin\codex-hosts.exe` 複製到 `skill\codex-hosts\bin\codex-hosts.exe`，然後把 `skill\codex-hosts` 目錄複製到 `%USERPROFILE%\.codex\skills\codex-hosts`。

### 使用 Codex 提示詞安裝

```text
從 https://github.com/Torinomii/codex-hosts/releases/latest 下載最新版本並安裝 codex-hosts：自動識別目前環境的 Skill 安裝目錄，將完整的 codex-hosts Skill 和執行檔安裝到正確位置，並確認 Skill、執行檔及必要的設定檔存在。
```

### 從原始碼安裝

原始碼建置需要 Rust 1.92 或更新版本以及 MSVC 工具鏈：

```
git clone https://github.com/Torinomii/codex-hosts.git
cd codex-hosts
cargo build --locked --release
```

將 `target\release\codex-hosts.exe` 複製到 `skill\codex-hosts\bin\codex-hosts.exe`，然後把完整的 `skill\codex-hosts` 目錄安裝為 `%USERPROFILE%\.codex\skills\codex-hosts`。

### 如何使用

1. 正確安裝 skill 和執行檔後，Codex 遇到新的遠端連線需求時會開啟 codex-hosts，要求輸入密碼。
2. 也可以事先在 codex-hosts 中輸入連線資訊，再讓 Codex 識別並使用已儲存的資訊進行連線。

## Codex 整合

存放庫在 `skill\codex-hosts` 中提供可攜式 Codex skill。安裝後，`SKILL.md` 會相對於已安裝的 skill 目錄尋找 `bin\codex-hosts.exe`，不需要修改與存放庫位置相關的絕對路徑。

GUI 編輯模式允許傳入非機密的預填資訊：

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

切勿透過對話、命令列引數、JSON、指令碼或日誌傳遞密碼或私密金鑰密碼片語。使用者只能在 GUI 的遮罩輸入欄位中輸入認證。

工具模式透過同一個執行檔讀取不含認證的要求檔案並寫入結果檔案：

```json
{"action":"list_hosts"}
{"action":"probe","alias":"example"}
{"action":"exec","alias":"example","command":"hostname"}
```

`probe` 和 `exec` 可以針對單次作業設定 `connect_timeout_ms` 與 `command_timeout_ms`；不需要應用程式層級逾時時應省略。已儲存的主機不包含固定逾時值，遠端命令會原樣傳送，不會猜測遠端作業系統。

## 安全界線

- 密碼與私密金鑰密碼片語只會儲存在 Windows 認證管理員中，絕不會傳回 Codex。
- SSH 指紋必須明確固定，應用程式不會自動取代。
- 只有已驗證的 SSH 設定檔才能作為跳板；跳板鏈會檢查循環並限制為最多八台主機。
- Telnet 是明文通訊協定，只應在明確接受該風險的受信任網路中使用。
- 每個輸出資料流最多擷取一 MiB 的命令輸出。
