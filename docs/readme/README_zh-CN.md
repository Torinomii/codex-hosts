<h1 align="center">codex-hosts</h1>

<p align="center">保存主机，掌握凭据，让 Codex 完成远程任务。</p>

<p align="center">
  <a href="https://github.com/Torinomii/codex-hosts/tree/v0.4.0"><img src="https://img.shields.io/badge/version-0.4.0-2563eb" alt="v0.4.0" /></a>
  <a href="https://github.com/Torinomii/codex-hosts/actions/workflows/release.yml"><img src="https://github.com/Torinomii/codex-hosts/actions/workflows/release.yml/badge.svg" alt="Build release" /></a>
  <a href="../../LICENSE"><img src="https://img.shields.io/badge/License-Apache--2.0-D22128.svg" alt="Apache 2.0 许可证" /></a>
  <img src="https://img.shields.io/badge/Windows-10%2B-0078D4?logo=windows&logoColor=white" alt="Windows 10 或更高版本" />
  <a href="https://linux.do/"><img src="https://img.shields.io/badge/LINUX-DO-FFB003" alt="LINUX DO Community" /></a>
</p>

<p align="center">
  <a href="../../README.md">English</a> · <a href="README_zh-CN.md">简体中文</a> · <a href="README_zh-TW.md">繁體中文</a> · <a href="README_ja.md">日本語</a>
</p>

<p align="center">
  <a href="https://github.com/Torinomii/codex-hosts/releases">下载</a> · <a href="#安装">安装</a> · <a href="#快速上手">快速上手</a> · <a href="#版本记录">版本记录</a> · <a href="#技术参考">技术参考</a>
</p>

供 Codex 使用的 Windows SSH / Telnet 主机管理工具与 MCP 服务器。将主机保存在本地、Bitwarden 或 1Password 中，就能让 Codex 连接、执行命令或批量操作多台机器。密码、私钥口令和 FIDO PIN 在本机输入，不进入对话或工具参数。

<p align="center">
  <img src="../../Main.png" alt="显示本地、Bitwarden 与 1Password 主机的 Codex Hosts 主界面" width="960" />
</p>

## 功能概览

| 能力 | 可以做什么 |
| --- | --- |
| 主机管理 | 保存带备注和标签的 SSH / Telnet 主机，支持搜索、筛选、导入和导出。 |
| 凭据存放 | 登录凭据可保存在 Windows 凭据管理器、Bitwarden 文件夹或 1Password 保险库。 |
| 多种认证 | 密码、OpenSSH 私钥、FIDO / YubiKey、Windows OpenSSH Agent 和 Pageant。 |
| 远程命令 | 执行命令、发送 stdin，或用 sudo / su 以明确指定的用户运行程序。 |
| 并发与批量 | 在同一 SSH 连接上运行多条命令，或对明确指定的主机列表执行。 |
| 连接控制 | 验证主机指纹和跳板链；按主机选择逐次认证、会话保持或空闲后断开。 |
| 调用审批 | 可在每次 SSH 调用连接前进行审阅。 |
| 临时秘密 | 将 API Key、Token 只保存在内存中，并注入本地程序的环境变量。 |

## 0.4.0 更新亮点

- **Bitwarden 主机：** 所选文件夹中的每个登录项目都是一台主机（名称 = 别名，`ssh://` 或 `telnet://` URI = 连接地址，用户名和密码 = 登录凭据）。每次使用前都通过官方 `bw` CLI 重新读取，保管库中的修改在下一次调用时生效。
- **1Password 主机：** 所选 1Password 保险库中的登录项目同样作为主机，通过官方 `op` CLI 与 1Password 桌面 App 读取。解锁始终在 1Password 中完成。
- **在窗口中管理：** 保管库主机与本地主机使用同一个编辑器新建、编辑和删除。跳板主机、调用审批和高级设置保存在 `codex-hosts` 中。

[查看版本差异](https://github.com/Torinomii/codex-hosts/compare/v0.3.3...v0.4.0)

## 安装

最快的方式是让 Codex 安装。把以下内容粘贴给 Codex：

```text
将 https://github.com/Torinomii/codex-hosts.git 克隆到固定目录，运行 scripts/install-local-skill.ps1 检查编译环境；编译、安装软链接和替换旧路径前都先让我确认。
确认已安装的整个 codex-hosts Skill 目录是一个符号链接、可执行文件解析到发布版构建，按 SKILL.md 在 config.toml 中注册 codex-hosts MCP 服务器，并保留源码目录。
```

Codex 会克隆仓库、运行引导式安装程序（编译、创建链接或替换任何内容前都会征求你的确认），并注册 MCP 服务器。此方式从源码构建，因此需要下表中“源码构建”的环境。如需手动安装，请按表格后的两步操作。

| 方式 | 所需环境 | 入口 |
| --- | --- | --- |
| 让 Codex 安装（推荐） | Codex + “源码构建”的环境 | 上方提示词 |
| GitHub Release | 64 位 Windows 10+、PowerShell 7.2+ | `install.cmd` 或下方命令 |
| 源码构建 | 上述环境，加 Rust 1.92+、MSVC 和 Windows SDK | `scripts/install.cmd` 或 `scripts/install-local-skill.ps1` |

### 1. 安装程序包

从 [Releases 页面](https://github.com/Torinomii/codex-hosts/releases)下载 `codex-hosts-windows-x86_64.zip`，用同处发布的 `.sha256` 核对（`(Get-FileHash .\codex-hosts-windows-x86_64.zip).Hash`），解压到固定目录后运行：

```powershell
pwsh -NoProfile -File .\install-local-skill.ps1 -ReleasePackage
```

双击 `install.cmd` 会启动同一个引导式安装程序。它会检查环境，并在替换旧安装前征求确认。

**请勿移动解压后的目录。** 已安装的 Skill 是指向它的符号链接，因此 Windows 必须允许当前用户创建符号链接。

### 2. 注册 MCP 服务器

在 `%USERPROFILE%\.codex\config.toml`（或工作区的 `.codex\config.toml`）中加入以下内容，并把 `<user>` 换成你的 Windows 账户名：

```toml
[mcp_servers.codex-hosts]
command = "C:\\Users\\<user>\\.codex\\skills\\codex-hosts\\bin\\codex-hosts.exe"
args = ["--mcp"]
startup_timeout_sec = 20
tool_timeout_sec = 86400
```

<details>
<summary><strong>从源码构建</strong></summary>

```powershell
git clone https://github.com/Torinomii/codex-hosts.git
cd codex-hosts
pwsh -NoProfile -File .\scripts\install-local-skill.ps1
```

脚本会检查 PowerShell、Rust、MSVC 工具链和 Windows SDK，说明缺少的部分，并在编译和创建链接前分别征求确认。已安装的 Skill 链接到 `skill\codex-hosts`，Skill 中的 `bin\codex-hosts.exe` 链接到构建产物，因此重新构建即可更新已安装的程序。被替换的路径保留为 `.codex-hosts.backup-*`。

手动构建时，先运行 `cargo build --locked --release`，再加 `-SkipBuild` 安装。

无法创建符号链接时，请开启开发者模式或使用管理员 PowerShell；脚本不会自行修改系统设置。

| 参数 | 作用 |
| --- | --- |
| `-CodexHome <directory>` | 安装到另一个 Codex 目录，而非 `CODEX_HOME` / `%USERPROFILE%\.codex` |
| `-NonInteractive -Build -Install` | 无人值守构建并安装；替换旧安装需另加 `-ReplaceExisting` |
| `-SkipBuild` / `-SkipInstall` | 使用已有构建 / 只构建 |

</details>

## 快速上手

### 1. 添加主机

打开 `codex-hosts.exe`，选择“新建主机”，填写别名、地址、端口、用户名、协议和认证方式，然后保存。

| 认证方式 | 使用 |
| --- | --- |
| Password | 保存在 Windows 凭据管理器或 Bitwarden / 1Password 项目中的密码 |
| OpenSSH Key | OpenSSH 私钥文件 |
| FIDO / 安全密钥 | 硬件密钥上的 OpenSSH FIDO Handle |
| SSH Agent | 已加载到 Windows OpenSSH Agent 或 Pageant 的身份 |

### 2. 可选：把主机放在 Bitwarden 或 1Password

在工具栏打开“凭据服务”，选择“开始设置…”。设置完成后，“新建主机”也能在该服务中新建主机。

| 服务 | 需要 | 哪些项目是主机 |
| --- | --- | --- |
| Bitwarden | 官方 Bitwarden CLI（`bw.exe`）；bitwarden.com、bitwarden.eu 或自托管服务器上的账户 | 一个文件夹中的登录项目 |
| 1Password | 开启“设置 › 开发者 › 与 1Password CLI 集成”的 1Password 桌面 App，以及官方 1Password CLI（`op.exe`） | 一个保险库中的登录项目；建议使用专用保险库 |

### 3. 让 Codex 使用主机

告诉 Codex 已保存的别名和任务：

```text
连接 example 并执行 hostname。
检查 web-1 和 web-2 的磁盘使用情况。
```

Skill 会查找主机、验证主机指纹、完成认证并返回结构化结果；你无需手动调用 MCP 工具。

## 安全边界

| 控制项 | 行为 |
| --- | --- |
| 登录凭据 | 保存在 Windows 凭据管理器或主机的 Bitwarden / 1Password 项目中，从不写入主机配置、工具参数、命令行或结果。FIDO PIN 不保存。 |
| 密码管理器 | Bitwarden 主密码只在 `codex-hosts` 中输入；1Password 只在 1Password App 中解锁。 |
| 主机身份 | 未知或变化的 SSH 主机指纹需要确认，跳板链中的每一跳也一样。 |
| 会话保持 | 保持的会话会跳过后续认证和硬件密钥触碰；调用审批是独立设置。 |
| sudo / su | 每次调用指定执行用户；供给保存的密码需要单独授权。 |
| 临时秘密 | 只保存在内存中，每个字段默认需要审批；退出托盘程序后清空。 |

> **Telnet 不提供加密。** 凭据、命令和输出可能以明文传输。仅在可信网络中使用，优先选择 SSH。

<details>
<summary><strong>凭据存储与密码管理器</strong></summary>

本地主机的密码和私钥口令保存在当前登录 Windows 账户的凭据管理器中。FIDO PIN 只用于当次操作。

主机库（`hosts.json`）必须属于运行 `codex-hosts` 的 Windows 账户。否则 MCP 服务器拒绝启动，并在日志中给出修复用的 `icacls /setowner` 命令；窗口只显示警告。

Bitwarden / 1Password 主机就是其登录项目本身：别名、连接地址、用户名、密码、备注和标签保存在项目中，其余设置保存在 `codex-hosts`。在窗口中保存主机会写入项目；删除主机会把 Bitwarden 项目移到回收站、把 1Password 项目归档，两者都可恢复。若项目在此期间已在保管库中被改动，编辑不会覆盖它。

- **Bitwarden：** “重新验证”决定保管库保持解锁多久：每次调取、闲置达到设定时间、直到 `codex-hosts` 关闭，或直到你锁定。保留的会话密钥存于 Windows 凭据管理器，结束时也会执行 `bw lock`。
- **1Password：** `codex-hosts` 从不接触你的账户密码。由 1Password App 请求你允许 codex-hosts，并由它决定授权的有效期。

</details>

<details>
<summary><strong>主机指纹与认证保持</strong></summary>

首次连接时，`codex-hosts` 显示服务器的主机指纹，确认后才保存。指纹变化时不会自动替换。

每台主机在“身份验证”卡片中选择 Codex 保持已认证会话的方式：

| 选项 | 行为 |
| --- | --- |
| 每次操作重新认证 | 调用结束即断开连接。旧版本主机与导入的主机使用此项。 |
| Codex 会话期间保持 | 新建主机的建议选项。会话保持到 Codex 退出、链路断开或调用 `disconnect`。 |
| 空闲后断开 | 空闲达到设定的分钟数后关闭会话。 |

会话保持期间，该主机后续的命令无需再次认证或触碰安全密钥。编辑器会在首次保存和每次更改时要求确认此选项，列表中这类主机带有标记。Telnet 主机始终重新认证。

</details>

<details>
<summary><strong>临时秘密</strong></summary>

`codex-hosts` 还能保存与主机登录无关的 API Key、Token 等值。它们只存在于 `codex-hosts` 进程内存中，不会返回给 Codex，而是注入 Codex 运行的本地程序的环境变量。

每个字段默认需要审批：使用它的程序启动前会弹窗询问。可按字段关闭审批以直接运行。退出托盘程序、注销或重启 Windows 后这些值失效；Codex 重启不会清除它们。

见 [`temporary-secrets.md`](../../skill/codex-hosts/references/temporary-secrets.md)。

</details>

## 版本记录

<details>
<summary><strong>0.4.0 · Bitwarden 与 1Password 主机</strong></summary>

- **Bitwarden 主机：** 一个文件夹中的登录项目即主机，备注和标签存于 `codex-hosts:` 自定义字段。每次使用前，官方 `bw` CLI 在不超过所选时限的同步之后重新读取项目；在文件夹中新建的主机可立即按别名找到。
- **Bitwarden 解锁：** 可选择保管库保持解锁的时长（每次调取、闲置时间、直到退出、直到锁定）。支持 bitwarden.com、bitwarden.eu、自托管服务器和新设备验证。
- **1Password 主机：** 一个保险库中的登录项目即主机，通过官方 `op` CLI 与 1Password 桌面 App 读取。设置时自动查找 `op.exe`，也可新建保险库。
- **编辑：** 保管库主机与本地主机使用同一个编辑器新建、编辑和删除；若保管库中同时有改动，会提示而不覆盖。
- **Codex：** `list_hosts` 返回 `credential_source`，新增的 `BITWARDEN_*` / `ONEPASSWORD_*` 错误码说明用户需要做什么。sudo 和 su 复用从项目读取的密码。

[查看版本差异](https://github.com/Torinomii/codex-hosts/compare/v0.3.3...v0.4.0)

</details>

<details>
<summary><strong>0.3.3 · sudo / su、调用审批与引导安装</strong></summary>

- **sudo / su：** `exec_sudo` 和 `exec_su` 以明确指定的用户运行绝对路径程序和原样参数。高级设置中的独立开关授权供给保存的密码；关闭时仍可免密执行。
- **调用审批：** 可选的按主机开关会在每次 SSH 调用（包括探测、切换用户和批量）连接前审阅。保持的连接也不会复用审批。
- **临时秘密：** 每个字段有自己的审批开关，默认开启。保存或清除字段会撤销等待中的执行。
- **凭据安全：** 保存的密码与账户绑定；更换登录账户绝不会把另一账户的密码交出。
- **安装：** Release 与源码安装都只使用符号链接，通过带环境检查和备份的分步脚本完成。

[查看版本差异](https://github.com/Torinomii/codex-hosts/compare/v0.3.2...v0.3.3)

</details>

<details>
<summary><strong>0.3.2 · 完整 Skill 目录链接</strong></summary>

- 已安装的 `codex-hosts` Skill 链接到整个源 Skill 目录，`SKILL.md`、`agents` 和 `references` 保持在一起。

[查看版本差异](https://github.com/Torinomii/codex-hosts/compare/v0.3.1...v0.3.2)

</details>

<details>
<summary><strong>0.3.1 · MCP 工具、认证保持与连接可靠性</strong></summary>

- **MCP 服务器：** `codex-hosts.exe --mcp` 通过 stdio 为 Codex 提供服务，所有工具都带风险注解。旧的基于文件的工具模式继续可用。
- **认证保持：** 每台主机可选择每次调用重新认证、在 Codex 会话期间保持，或空闲后断开。
- **编辑器：** 标记必填项；“高级”卡片提供按主机的调优（通道数、超时、keepalive、远端环境、对 Codex 隐藏、Telnet 提示符）。
- 取消 Codex 调用会关闭通道；长时间工作应放在远端主机的 `tmux` / `screen` 中。
- 改进 Telnet 登录、跳板清理和空闲计时的可靠性；Release 附带 SHA-256 校验和。

[查看版本差异](https://github.com/Torinomii/codex-hosts/compare/v0.3.0...v0.3.1)

</details>

<details>
<summary><strong>0.3.0 · 主机界面重设计、备注与标签</strong></summary>

- **备注与标签：** 按备注或标签搜索、筛选；`list_hosts` 返回这两项并支持 `tags` 筛选，`exec` 接受 `stdin`。
- **界面重设计：** 可调整大小的主机列表、分组的主机详情、状态栏与通知、批量选择，以及浅色和深色主题。

[查看版本差异](https://github.com/Torinomii/codex-hosts/compare/v0.2.3...v0.3.0)

</details>

## 技术参考

<details>
<summary><strong>MCP 工具</strong></summary>

每个工具都以 `structuredContent` 和文本两种形式返回相同的 JSON，并带有区分只读工具与执行代码工具的 MCP 注解。

| 工具 | 用途 |
| --- | --- |
| `list_hosts` | 已保存主机的非敏感信息、`credential_source`、信任状态、`auth_persistence`、`require_call_approval`、sudo/su 权限、`max_channels`、`remote_env` |
| `agent_identities`、`fido_identities` | 已加载 Agent 密钥和 FIDO Handle 的公开信息 |
| `probe`、`batch_probe` | 认证并执行 `hostname`，报告未知或变化的主机指纹 |
| `exec`、`exec_stdin`、`exec_many`、`batch_exec` | 执行命令；stdin 内容无法像命令一样审阅，因此 `exec_stdin` 单独提供 |
| `exec_sudo`、`exec_su` | 以每次调用指定的 `user` 运行绝对路径程序和原样参数 |
| `disconnect` | 断开保持的会话 |
| `open_host_editor` | 打开预填非敏感信息的编辑器，或确认报告的主机指纹 |
| `temporary_secrets_open`、`_status`、`_run`、`_clear` | 托盘程序保存的内存秘密 |

参数中从不包含密码、口令或 PIN。超时（`connect_timeout_ms`、`command_timeout_ms`、`batch_timeout_ms`）按调用设置，默认连接 120 秒、每条命令 10 分钟，或采用主机高级设置中的默认值。`open_host_editor` 的认证方式为 `password`、`private-key`（也用于 FIDO Handle）和 `ssh-agent`。

**每次 Codex 调用前审批**（高级设置，默认关闭）会在连接前显示该次调用的主机和命令。一次审批只覆盖一次调用（包括其批量中的所有主机）；即使连接仍保持，下一次调用也会再次询问。未处理的审批在五分钟后到期。见 [approvals.md](../../skill/codex-hosts/references/approvals.md)。

**sudo / su：** Codex 每次调用都必须指定执行用户（root 用 `"user":"root"`）。高级设置中的“允许 sudo 使用保存的密码”和“允许 su 使用保存的密码”分别授权供给保存的密码：sudo 复用 SSH 登录密码或单独保存的密码，su 使用目标账户的密码。开关关闭时仍可免密执行，要求输入密码时以 `SUDO_AUTO_AUTH_DISABLED` 或 `SU_AUTO_AUTH_DISABLED` 失败。只处理最外层认证，脚本内部的 sudo 不会获得密码。远端要求见 [sudo.md](../../skill/codex-hosts/references/sudo.md) 和 [su.md](../../skill/codex-hosts/references/su.md)。

完整的 Codex 行为与安全规则见 [`SKILL.md`](../../skill/codex-hosts/SKILL.md)。

</details>

<details>
<summary><strong>多命令与多主机</strong></summary>

`exec_many` 在同一 SSH 连接上并发运行多条短命令，因此安全密钥只需触碰一次：

```json
{"alias":"example","commands":["hostname","uptime","df -h"],"max_concurrency":8}
```

`batch_exec` 和 `batch_probe` 接受明确的主机列表；空列表绝不表示全部主机：

```json
{"aliases":["web-1","web-2","web-3"],"command":"uptime","max_concurrency":8,"batch_timeout_ms":30000}
```

`exec_stdin` 将原样的 UTF-8 文本（最多 1 MiB）发送给单台 SSH 主机上的命令，然后发送 EOF：

```json
{"alias":"example","command":"python3 -","stdin":"print('hello')\n","command_timeout_ms":10000}
```

</details>

<details>
<summary><strong>备注、标签与 CSV</strong></summary>

主机可保存多行 `description` 和任意数量的 `tags`。搜索匹配别名、地址、用户名、备注和标签；选择多个标签筛选时必须全部匹配。`list_hosts` 返回这两项并接受 `tags` 筛选，`open_host_editor` 可预填它们。

CSV 导入导出包含可选的 `description` 和 `tags` 列；标签单元格是 `["prod","web"]` 这样的 JSON 数组。

</details>

<details>
<summary><strong>限制、超时与长时间任务</strong></summary>

- 单条命令最多捕获 **1 MiB** 输出；`exec_many` 或批量结果最多 **8 MiB**。请检查 `output_truncated`。
- `tool_timeout_sec` 只是上限；每次调用都带有自己更短的超时。
- 远程命令重复执行不一定安全，因此从不自动重试。
- 取消 Codex 调用会关闭通道并返回 `CANCELLED`；已经开始的命令可能继续运行。
- 超过几分钟的工作应放进远端主机的 `tmux` / `screen`，见 [`long-running.md`](../../skill/codex-hosts/references/long-running.md)。

`probe` 和 `batch_probe` 标记为只读，但仍会认证，因此可能需要触碰安全密钥。如需 Codex 也确认这两个工具：

```toml
[mcp_servers.codex-hosts.tools.probe]
approval_mode = "prompt"

[mcp_servers.codex-hosts.tools.batch_probe]
approval_mode = "prompt"
```

</details>

<details>
<summary><strong>FIDO / 安全密钥、SSH Agent 与跳板主机</strong></summary>

- **FIDO：** 无需 Agent 即可使用现有的 OpenSSH ECDSA-SK 和 Ed25519-SK Handle（`id_ecdsa_sk`、`id_ed25519_sk`）；私钥留在设备上，可能需要 PIN 或触碰。程序中新建的凭据使用 ECDSA-SK。
- **SSH Agent：** 认证交给已在运行的 Windows OpenSSH Agent 或 Pageant；`codex-hosts` 从不启动或配置 Agent。
- **跳板主机：** SSH 主机可经由其他已保存且已验证的 SSH 主机连接，链最多 8 台。每一跳都验证主机指纹，拒绝循环，Agent 转发始终关闭。

</details>

## License

本项目使用 [Apache License 2.0](../../LICENSE)。
