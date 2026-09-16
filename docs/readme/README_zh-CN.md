# codex-hosts

<p align="center">
  <a href="https://github.com/Torinomii/codex-hosts/actions/workflows/release.yml"><img src="https://github.com/Torinomii/codex-hosts/actions/workflows/release.yml/badge.svg" alt="构建 Release" /></a>
  <a href="../../LICENSE"><img src="https://img.shields.io/badge/License-Apache--2.0-D22128.svg" alt="Apache 2.0 许可证" /></a>
  <img src="https://img.shields.io/badge/Windows-10%2B-0078D4?logo=windows&logoColor=white" alt="Windows 10 或更高版本" />
  <img src="https://img.shields.io/badge/Rust-1.92%2B-000000?logo=rust&logoColor=white" alt="Rust 1.92 或更高版本" />
  <a href="https://linux.do/"><img src="https://img.shields.io/badge/LINUX-DO-FFB003" alt="LINUX DO 社区" /></a>
</p>

[English](../../README.md) | [简体中文](README_zh-CN.md) | [繁體中文](README_zh-TW.md) | [日本語](README_ja.md)

`codex-hosts` 是一个供 Codex 使用的 Windows SSH / Telnet 主机管理工具，让 Codex 可以连接和操作远程主机，而无需在对话、命令参数或请求文件中直接处理密码、私钥口令和 FIDO PIN 等敏感凭据。

![Codex Hosts 主界面](../../Main.png)

## 主要功能

- 管理可复用的 SSH / Telnet 主机配置。
- 支持密码和普通 OpenSSH 私钥认证。
- 支持 FIDO / YubiKey 等硬件安全密钥。
- 支持 Windows OpenSSH Agent 和 Pageant 中已经加载的身份。
- SSH 主机指纹固定与人工确认。
- 支持经过验证的 SSH Jump Host / 跳板链。
- 支持单主机命令执行。
- 支持 SSH 单连接并发执行多条命令。
- 支持 SSH / Telnet 多主机批量探测和执行。
- 支持内存中的临时秘密，可用于 API Key、Token 等不适合直接交给 Codex 的敏感参数。
- 提供完整 Codex Skill，可由 Codex 自动完成主机查找、连接、认证和命令执行。

## 安装

### 直接下载

预编译版本支持 64 位 Windows 10 或更高版本，可从 [Releases](https://github.com/Torinomii/codex-hosts/releases/latest) 下载。

### 安装 Codex Skill

完整 Skill 的安装结构：

```text
%USERPROFILE%\.codex\skills\codex-hosts\
├── SKILL.md
├── bin\
│   └── codex-hosts.exe
├── agents\
└── references\
```

手动安装：

1. 将 Release 中的 `codex-hosts.exe` 放到：

```text
%USERPROFILE%\.codex\skills\codex-hosts\bin\codex-hosts.exe
```

2. 将 Release 中完整的 `skill\codex-hosts` 内容复制到：

```text
%USERPROFILE%\.codex\skills\codex-hosts
```

不要只复制 `SKILL.md` 或可执行文件，应保留完整 Skill 目录。

也可以直接让 Codex 安装：

```text
从 https://github.com/Torinomii/codex-hosts/releases/latest 下载并安装最新版 codex-hosts。
请自动找到当前环境的 Skill 安装目录，安装完整的 Skill 和可执行文件，并确认所需文件都已就位。
```

### 从源代码构建

需要 Rust 1.92 或更高版本以及 MSVC 工具链：

```powershell
git clone https://github.com/Torinomii/codex-hosts.git
cd codex-hosts
cargo build --locked --release
```

生成的可执行文件位于：

```text
target\release\codex-hosts.exe
```

构建完成后，将它放入 Skill 的 `bin` 目录即可。

## 快速上手

### 1. 添加主机

打开 `codex-hosts.exe`，新建一个主机。

填写：

- 别名
- 地址或 IP
- 端口
- 用户名
- 协议
- 认证方式

然后保存。

### 2. 选择认证方式

| 认证方式 | 说明 |
| --- | --- |
| Password | 使用密码登录，密码保存在 Windows 凭据管理器中 |
| OpenSSH Key | 使用普通 OpenSSH 私钥文件 |
| FIDO / 安全密钥 | 使用 OpenSSH FIDO Handle，通过硬件设备完成签名 |
| SSH Agent | 使用已经加载到 Windows OpenSSH Agent 或 Pageant 中的身份 |

这些认证方式中的敏感值不会通过 Codex 对话传递。

### 3. 让 Codex 使用主机

保存主机后，只需要告诉 Codex 主机别名和需要执行的任务。

例如：

```text
连接 example，执行 hostname。
```

或者：

```text
检查 web-1 和 web-2 的磁盘使用情况。
```

也可以一次执行多个操作：

```text
连接 server1，检查 hostname、uptime 和磁盘空间。
```

Codex Skill 会负责：

- 查找主机配置
- 建立连接
- 验证 SSH Host Key
- 完成认证
- 执行命令
- 返回结构化结果

正常使用时不需要手动编写 Tool JSON。

## 安全边界

### 登录凭据

密码和私钥文件口令会持久化保存在 Windows 凭据管理器中。

这些敏感值不会：

- 写入主机配置
- 放入 Tool JSON
- 作为命令行参数传递
- 返回给 Codex

FIDO PIN 只用于当前操作，不会保存。

### SSH Host Key

SSH 主机指纹必须由用户明确确认。

首次连接未知主机时，`codex-hosts` 会显示实际检测到的主机指纹。

只有用户确认后才会保存。

如果服务器 Host Key 后续发生变化，程序不会自动替换已经保存的指纹，必须再次由用户明确确认。

### 临时秘密

`codex-hosts` 还可以临时保存 API Key、Token 等与主机登录无关的敏感参数。

这类值只保存在当前 `codex-hosts` 进程内存中，不保存到 Windows 凭据管理器，也不会返回给 Codex。

使用时可以在用户批准后直接注入指定程序的环境变量。

退出 `codex-hosts`、注销或重启系统后，这些临时值会失效。

完整说明见 [`temporary-secrets.md`](../../skill/codex-hosts/references/temporary-secrets.md)。

## FIDO / 安全密钥

`codex-hosts` 可以使用现有的 OpenSSH ECDSA-SK 和 Ed25519-SK FIDO Handle。

例如：

```text
id_ecdsa_sk
id_ed25519_sk
```

使用应用内创建或恢复新的 FIDO SSH 凭据时，使用 ECDSA-SK。

FIDO 模式下：

- 硬件私钥不会离开安全设备。
- FIDO PIN 只用于当前操作，不会保存。
- 不要求启用 SSH Agent。
- Agent Forwarding 始终关闭。
- 根据安全密钥配置，认证过程中可能需要 PIN 或 Touch。

FIDO Handle 和 SSH Agent 是两种不同的认证方式。

FIDO Handle：

```text
codex-hosts
    │
    ▼
OpenSSH FIDO Handle
    │
    ▼
安全密钥
```

由 `codex-hosts` 直接通过系统组件完成硬件签名。

SSH Agent：

```text
codex-hosts
    │
    ▼
OpenSSH Agent / Pageant
    │
    ▼
已加载的身份
```

认证工作交给已经运行的 Agent。

`codex-hosts` 不会为了使用 Agent 模式而自动启动、启用或持久化 Agent 服务。

## Jump Host

SSH 主机可以使用其他已经保存并验证的 SSH 主机作为 Jump Host。

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

安全规则：

- Jump Host 必须是已经验证的 SSH 主机。
- 每个 Hop 都需要验证 Host Key。
- 不允许 Jump Host 链循环。
- 一条 SSH 链最多包含 8 台主机。
- Agent Forwarding 始终关闭。

## Telnet

`codex-hosts` 同样支持 Telnet。

但 Telnet 本身没有加密，用户名、密码、命令和返回数据都可能以明文形式在网络中传输。

因此只应在你明确接受该风险的可信网络中使用 Telnet。

互联网环境建议使用 SSH。

## 批量执行

### SSH 单主机多命令

`exec_many` 可以复用同一条 SSH 连接，同时执行多条独立短命令：

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

它只进行一次 SSH 认证，然后通过同一条连接创建多个 Channel。

对于 FIDO / 安全密钥尤其有用，因为一组命令通常可以共用一次身份认证。

`exec_many` 仅用于 SSH 主机。

### 多主机批量执行

可以显式指定多个主机：

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

也可以批量测试连接：

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

批量模式要求显式指定主机列表。

空列表不会被解释为“全部主机”。

<details>
<summary>Codex / Tool 接口</summary>

### GUI 编辑模式

可以从 Codex 或脚本打开主机编辑器，并预填非敏感连接信息：

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

不要将密码、私钥口令或 FIDO PIN 作为参数传递。

认证参数使用稳定名称：

- `password`：密码认证。
- `private-key` / `private_key`：普通私钥文件或 FIDO Handle，也接受 `fido-handle`。
- `ssh-agent` / `ssh_agent`：Windows OpenSSH Agent 或 Pageant。

`private-key` 同时用于普通 OpenSSH 私钥和 FIDO Handle。

`ssh-agent` 只表示 Agent / Pageant 模式，并不代表所有硬件密钥认证方式。

### Tool 模式

Tool 模式通过 UTF-8 JSON 请求文件和结果文件通信，请求和结果文件中不得包含凭据。

常见请求：

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

`agent_identities` 和 `fido_identities` 只返回公开身份信息和公钥。

执行远程命令时，请检查结果中的 `output_truncated`，确认输出是否因为长度限制被截断。

完整的 Codex 行为、调用流程和安全规则见 [`SKILL.md`](../../skill/codex-hosts/SKILL.md)。

</details>

## 执行限制

为了避免异常命令产生无限输出或占用过多内存，命令执行存在明确限制。

- 单条命令最多捕获 **1 MiB** 输出。
- 完整 `exec_many` 或批量 JSON 结果最大为 **8 MiB**。
- `exec_many` 和批量任务使用受限并发。
- 输出被截断时，可通过 `output_truncated` 判断。
- 远程命令不会因为网络错误自动重新执行。

不自动重试是因为远程命令可能不是幂等操作，例如：

```text
reboot
rm
systemctl restart
数据库写入
部署操作
```

网络异常并不意味着同一命令可以安全执行第二次。

## 项目结构

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

本项目使用 [Apache License 2.0](../../LICENSE)。
