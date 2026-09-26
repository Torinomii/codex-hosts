# codex-hosts

<p align="center">
  <a href="https://github.com/Torinomii/codex-hosts/actions/workflows/release.yml"><img src="https://github.com/Torinomii/codex-hosts/actions/workflows/release.yml/badge.svg" alt="构建 Release" /></a>
  <a href="../../LICENSE"><img src="https://img.shields.io/badge/License-Apache--2.0-D22128.svg" alt="Apache 2.0 许可证" /></a>
  <img src="https://img.shields.io/badge/Windows-10%2B-0078D4?logo=windows&logoColor=white" alt="Windows 10 或更高版本" />
  <img src="https://img.shields.io/badge/Rust-1.92%2B-000000?logo=rust&logoColor=white" alt="Rust 1.92 或更高版本" />
  <a href="https://linux.do/"><img src="https://img.shields.io/badge/LINUX-DO-FFB003" alt="LINUX DO 社区" /></a>
</p>

[English](../../README.md) | [简体中文](README_zh-CN.md) | [繁體中文](README_zh-TW.md) | [日本語](README_ja.md)

`codex-hosts` 是一个供 Codex 使用的 Windows SSH / Telnet 主机管理工具。它以 MCP 服务器的形式运行，Codex 通过结构化工具连接和操作远程主机，密码、私钥口令和 FIDO PIN 等敏感凭据从不出现在对话、命令参数或文件中。

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
- 带风险注解的 MCP 服务器，加上一份 Codex Skill，覆盖主机查找、连接、认证和命令执行。
- 按主机设置认证保持：Codex 会话期间保持（新建主机预选，首次保存时确认）、每次操作重新认证，或空闲后断开。

## 0.3.1 更新内容

- MCP 服务器：`codex-hosts.exe --mcp` 通过 stdio 为 Codex 提供服务。主机、探测、命令、批量、主机编辑器和临时秘密都是带风险注解的 MCP 工具，不再经过 shell、PowerShell 包装或临时请求文件。原有的文件式 Tool 模式原样保留，尚未更新的 Skill 仍可正常工作。
- 认证保持：每台主机现在都要选择 Codex 是在 Codex 会话期间保持会话（新建主机预选；首次保存和每次更改都会要求确认）、每次操作重新认证（原有行为，旧版本主机与导入的主机保持此项），还是空闲指定分钟后断开。保持会话的主机在列表中带有标记，编辑器中显示警告。
- 编辑器：必填项标有 `*`，保存时留空会被高亮；新增"高级"卡片，提供按主机的并发通道数、默认超时、Keepalive 间隔、供 Codex 参考的远端环境提示、对 Codex 隐藏主机以及 Telnet 提示符覆盖。
- 取消 Codex 调用会立即停止等待并关闭通道；长时间作业的做法改为依赖远端主机自己的 `tmux` / `screen`，而不是长时间等待。
- `codex-hosts.exe --version` 输出版本号，参数错误会写到 stderr。

## 安装

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

下载 Release 请按下文的安装步骤操作。若从源码构建，则在源码目录运行链接安装脚本：

```powershell
pwsh -NoProfile -File .\scripts\install-local-skill.ps1
```

脚本会把已安装的整个 `codex-hosts` Skill 目录作为一个符号链接指向 `skill\codex-hosts`；源码 Skill 目录内的 `bin\codex-hosts.exe` 再链接到发布版构建。请保留源码目录；移动或删除后链接会失效。当前 Windows 用户须有创建符号链接的权限。MCP 服务器仍须在 `%USERPROFILE%\.codex\config.toml`（或工作区的 `.codex\config.toml`）中注册一次：

```toml
[mcp_servers.codex-hosts]
command = "C:\\Users\\<user>\\.codex\\skills\\codex-hosts\\bin\\codex-hosts.exe"
args = ["--mcp"]
startup_timeout_sec = 20
tool_timeout_sec = 86400
```

`tool_timeout_sec` 只是上限；每次调用都带有自己的超时。如果从不使用阻塞等待处理长作业，可以保留 Codex 的默认值。

`probe` 与 `batch_probe` 标注为只读，Codex 会不经确认（且可并行）直接运行它们：它们只执行 `hostname`，而无人值守地预先认证一台保持会话的主机正是这个设计的价值所在。它们仍会连接并认证，因此硬件密钥会要求触碰。若希望每次都确认，可在 Codex 中这样设置：

```toml
[mcp_servers.codex-hosts.tools.probe]
approval_mode = "prompt"

[mcp_servers.codex-hosts.tools.batch_probe]
approval_mode = "prompt"
```

不要只安装 `SKILL.md` 或可执行文件；请保留完整的源码目录或解压后的 Release 目录。

也可以直接让 Codex 安装：

```text
将 https://github.com/Torinomii/codex-hosts.git 克隆到固定目录，运行 cargo build --locked --release，然后在源码目录运行 scripts/install-local-skill.ps1。
确认已安装的整个 codex-hosts Skill 目录是一个符号链接、可执行文件解析到发布版构建，按 SKILL.md 在 config.toml 中注册 codex-hosts MCP 服务器，并保留源码目录。
```

### 下载 GitHub Release

从 [Releases 页面](https://github.com/Torinomii/codex-hosts/releases)下载 `codex-hosts-windows-x86_64.zip`，解压到固定目录，然后在该目录运行：

```powershell
pwsh -NoProfile -File .\install-local-skill.ps1 -ReleasePackage
```

安装后的整个 Skill 目录链接到解压目录，因此请保留该目录。

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

构建完成后，运行下面的链接安装命令。

### 链接本地源码目录

如果在固定路径的本地源码目录中开发，可以安装软链接，之后每次构建无需再复制：

```powershell
pwsh -NoProfile -File .\scripts\install-local-skill.ps1
```

脚本会把已安装的 `codex-hosts` 目录整体链接到 `skill\codex-hosts`。源码 Skill 目录内的 `bin\codex-hosts.exe` 链接到 `target\release\codex-hosts.exe`，因此重新构建后无需重装。替换现有安装前会验证全部源文件，安装后会核对链接目标；如果安装失败，则恢复原有安装。可随时再次运行以验证同一套链接。当前 Windows 用户必须具备创建符号链接的权限。

从本地制作的分发 ZIP 安装时也使用同一脚本，加上 `-ReleasePackage`，并保留解压目录。

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
- 备注和标签（可选）

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

正常使用时不需要手动调用这些工具。

## 安全边界

### 登录凭据

密码和私钥文件口令会持久化保存在 Windows 凭据管理器中。

这些敏感值不会：

- 写入主机配置
- 放入 MCP 工具参数或结果
- 作为命令行参数传递
- 返回给 Codex

FIDO PIN 只用于当前操作，不会保存。

### SSH Host Key

SSH 主机指纹必须由用户明确确认。

首次连接未知主机时，`codex-hosts` 会显示实际检测到的主机指纹。

只有用户确认后才会保存。

如果服务器 Host Key 后续发生变化，程序不会自动替换已经保存的指纹，必须再次由用户明确确认。

### 认证保持

旧版本的主机和导入的主机在 Codex 的每次调用都重新认证：硬件密钥每个动作触碰一次，Codex 在两次调用之间不持有任何打开的会话。新建主机预选"Codex 会话期间保持"；编辑器会在首次保存时以及之后每次更改（无论改向哪一档）时要求确认。每台主机都可以在"认证"卡片中选择任意一项：

| 选项 | 行为 |
| --- | --- |
| 每次操作重新认证 | 调用结束即断开连接。旧版本主机与导入的主机从这一项开始。 |
| Codex 会话期间保持 | 新建主机预选。已认证的会话保持打开并发送 keepalive，直到 Codex 退出、链路断开或调用 `disconnect`。 |
| 空闲后断开 | 会话保持到空闲达到设定的分钟数为止。 |

保持会话期间，Codex 对该主机的后续命令不再重新认证或触碰安全密钥，也就取消了"每个动作一次确认"的保护。编辑器会显示这条警告，保持会话的主机在列表中带有标记，并且 Skill 禁止 Codex 建议修改这一设置。Telnet 主机始终每次重新认证。凭据、PIN 和主机密钥检查均不受影响，被复用的只是活动会话。

### 临时秘密

`codex-hosts` 还可以临时保存 API Key、Token 等与主机登录无关的敏感参数。

这类值只保存在当前 `codex-hosts` 进程内存中，不保存到 Windows 凭据管理器，也不会返回给 Codex。

使用时可以在用户批准后直接注入指定程序的环境变量。

退出 `codex-hosts` 托盘程序、注销或重启系统后，这些临时值会失效；Codex 的启动或退出不影响它们。

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
<summary>MCP 工具</summary>

Codex 通过 stdio 与 `codex-hosts.exe --mcp` 通信。每个工具都同时以 `structuredContent` 和文本返回同一份 JSON，并带有 MCP 注解，便于策略层区分只读工具和会执行代码的工具。

| 工具 | 用途 |
| --- | --- |
| `list_hosts` | 已保存主机的非敏感信息、信任状态、`auth_persistence`、`max_channels`、`remote_env` |
| `agent_identities`、`fido_identities` | 已加载 Agent 身份与 FIDO Handle 的公开信息 |
| `probe`、`batch_probe` | 认证并运行 `hostname`；暴露未知或变更的主机密钥 |
| `exec`、`exec_stdin`、`exec_many`、`batch_exec` | 执行命令；`exec_stdin` 单独成工具，因为 stdin 里的程序正文对审查不透明 |
| `disconnect` | 断开为已选择保持会话的主机保留的连接 |
| `open_host_editor` | 打开预填了非敏感信息的编辑器窗口，或确认上报的主机密钥 |
| `temporary_secrets_open`、`_status`、`_run`、`_clear` | 由托盘程序持有的仅内存秘密 |

参数中永远不包含密码、口令或 PIN；编辑器在遮罩输入框中收集它们。超时（`connect_timeout_ms`、`command_timeout_ms`、`batch_timeout_ms`）按调用传入，省略时默认连接 120 秒、单条命令 10 分钟，或取主机"高级"卡片中的默认值。

`open_host_editor` 的认证参数使用稳定名称：`password`；`private-key` / `private_key` 表示普通私钥文件或 FIDO Handle（也接受 `fido-handle`）；`ssh-agent` / `ssh_agent` 表示 Windows OpenSSH Agent 或 Pageant。

执行远程命令时请检查 `output_truncated`，判断输出是否因大小限制被截断。

完整的 Codex 行为、工作流和安全规则见 [`SKILL.md`](../../skill/codex-hosts/SKILL.md)。

</details>

## 主机备注、标签与命令输入

主机支持多行 `description` 备注和多个 `tags` 标签。编辑器可以添加、移除标签；标签会去除首尾空格、空项及不区分大小写的重复项，并保留首次输入的写法。仅修改备注或标签不会清除连接验证和主机密钥信任。搜索覆盖别名、地址、用户名、备注和标签；多个标签筛选条件必须全部匹配。批量全选仅选择可见主机，切换筛选会移除隐藏主机的选择。

`list_hosts` 返回这两个字段，并接受可选的 `tags` 数组。省略或传入空数组时返回全部主机；不存在的标签返回空列表。`open_host_editor` 可预填 `description` 和 `tags`。省略字段保留原值；空备注或单个空标签可清空对应字段。CSV 模板及导入导出增加可选的 `description`、`tags` 列；标签单元格采用 `["prod","web"]` 这样的 JSON 数组，按 CSV 规则引用。旧配置和旧 CSV 继续兼容。

```json
{"tags":["prod","web"]}
{"alias":"example","command":"python3 -","stdin":"print('hello')\n","command_timeout_ms":10000}
```

`exec_stdin` 向单台 SSH 主机的命令发送精确的 UTF-8 文本，上限 1 MiB，不追加换行，结束后发送 EOF；`""` 立即发送 EOF。输入发送和输出读取并发进行，并沿用原有超时及输出限制。远端程序可以在读完输入前退出，此时以远端退出状态为准。输入过大返回 `STDIN_TOO_LARGE`；Telnet 主机返回 `STDIN_UNSUPPORTED`。程序不会自动重放命令。

## 执行限制

为了避免异常命令产生无限输出或占用过多内存，命令执行存在明确限制。

- 单条命令最多捕获 **1 MiB** 输出。
- 完整 `exec_many` 或批量 JSON 结果最大为 **8 MiB**。
- `exec_many` 和批量任务使用受限并发。
- 输出被截断时，可通过 `output_truncated` 判断。
- 远程命令不会因为网络错误自动重新执行。
- 取消 Codex 调用会关闭通道并返回 `CANCELLED`；已经开始的远程命令可能继续运行。
- 超过几分钟的工作应放进远端主机的 `tmux` / `screen`，见 [`long-running.md`](../../skill/codex-hosts/references/long-running.md)。

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
