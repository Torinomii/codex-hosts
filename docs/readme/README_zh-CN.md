# codex-hosts

<p align="center">
  <a href="https://github.com/Torinomii/codex-hosts/actions/workflows/release.yml"><img src="https://github.com/Torinomii/codex-hosts/actions/workflows/release.yml/badge.svg" alt="构建 Release" /></a>
  <a href="../../LICENSE"><img src="https://img.shields.io/badge/License-Apache--2.0-D22128.svg" alt="Apache 2.0 许可证" /></a>
  <img src="https://img.shields.io/badge/Windows-10%2B-0078D4?logo=windows&logoColor=white" alt="Windows 10 或更高版本" />
  <img src="https://img.shields.io/badge/Rust-1.92%2B-000000?logo=rust&logoColor=white" alt="Rust 1.92 或更高版本" />
  <a href="https://linux.do/"><img src="https://img.shields.io/badge/LINUX-DO-FFB003" alt="LINUX DO 社区" /></a>
</p>

[English](../../README.md) | [简体中文](README_zh-CN.md) | [繁體中文](README_zh-TW.md) | [日本語](README_ja.md)

`codex-hosts` 是一个供 Codex 使用的 Windows SSH / Telnet 主机管理工具，让 Codex 无需接触密码或密钥即可安全发起连接。

![Codex Hosts 主界面](../../Main.png)

## 主要功能

- 保存服务器信息提供给codex连接。
- 支持密码、普通 OpenSSH 密钥或 FIDO/YubiKey 等硬件密钥登录。
- 支持 Windows OpenSSH Agent 或 Pageant 中加载的身份。

## 安装

### 直接下载

预编译版本支持 64 位 Windows 10 或更高版本，可从 [Releases](https://github.com/Torinomii/codex-hosts/releases/latest) 下载。

如果你手动安装：

1. 将 `bin\codex-hosts.exe` 放到 `skill\codex-hosts\bin\codex-hosts.exe`。
2. 将完整的 `skill\codex-hosts` 文件夹复制到 `%USERPROFILE%\.codex\skills\codex-hosts`。

也可以直接让 Codex 安装：

```text
从 https://github.com/Torinomii/codex-hosts/releases/latest 下载并安装最新版 codex-hosts。请自动找到当前环境的 Skill 安装目录，安装完整的 Skill 和可执行文件，并确认所需文件都已就位。
```

### 从源代码构建

需要 Rust 1.92 或更高版本以及 MSVC 工具链：

```powershell
git clone https://github.com/Torinomii/codex-hosts.git
cd codex-hosts
cargo build --locked --release
```

构建完成后，将 `target\release\codex-hosts.exe` 复制到 `skill\codex-hosts\bin\codex-hosts.exe`，再安装完整的 Skill 文件夹。

## 快速上手

1. 打开 `codex-hosts.exe`，新建一个主机。
2. 填写别名、地址、端口和用户名，再选择登录方式，保存。


<details>
<summary>接口</summary>

### Codex 或脚本调用

GUI 编辑模式可以预填不敏感的连接信息：

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

认证参数使用稳定名称：

- `password`：密码登录。
- `private-key` / `private_key`：密钥文件或 FIDO 句柄，也接受 `fido-handle`。
- `ssh-agent` / `ssh_agent`：正在运行的 SSH Agent 或 Pageant。

工具模式通过请求文件和结果文件工作，文件中不能包含凭据：

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

`exec_many` 会先登录一次，再通过同一条 SSH 连接同时运行多条短命令；使用硬件密钥时，这通常可以把一组命令合并为一次认证。`agent_identities` 和 `fido_identities` 只返回公开的身份信息与公钥。执行远程命令时，请检查结果中的 `output_truncated`，确认输出是否因为长度限制被截断。

</details>

## 安全边界

- 密码和密钥文件口令只保存在 Windows 凭据管理器中；FIDO PIN 只用于当前操作，不会保存。
- SSH 主机指纹必须由用户明确确认，程序不会自动替换已经保存的指纹。
- FIDO 直连不会启动或启用 Agent 服务；Agent 转发也始终关闭。
- 只有已经验证的 SSH 主机才能作为跳板，跳板链最多包含八台主机，并会检查循环。
- Telnet 会明文传输账号和数据，只应在你明确接受风险的可信网络中使用。
- 单条命令最多接收 1 MiB 输出；`exec_many` 或一组批量结果写成完整 JSON 后最多为 8 MiB。程序会在接收输出时直接执行预算，不会先在内存中保存多份大结果。
