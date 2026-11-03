<h1 align="center">codex-hosts</h1>

<p align="center">Your saved hosts. Your credentials. Codex at work.</p>

<p align="center">
  <a href="https://github.com/Torinomii/codex-hosts/tree/v0.4.0"><img src="https://img.shields.io/badge/version-0.4.0-2563eb" alt="v0.4.0" /></a>
  <a href="https://github.com/Torinomii/codex-hosts/actions/workflows/release.yml"><img src="https://github.com/Torinomii/codex-hosts/actions/workflows/release.yml/badge.svg" alt="Build release" /></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/License-Apache--2.0-D22128.svg" alt="Apache 2.0 License" /></a>
  <img src="https://img.shields.io/badge/Windows-10%2B-0078D4?logo=windows&logoColor=white" alt="Windows 10 or newer" />
  <a href="https://linux.do/"><img src="https://img.shields.io/badge/LINUX-DO-FFB003" alt="LINUX DO Community" /></a>
</p>

<p align="center">
  <a href="README.md">English</a> · <a href="docs/readme/README_zh-CN.md">简体中文</a> · <a href="docs/readme/README_zh-TW.md">繁體中文</a> · <a href="docs/readme/README_ja.md">日本語</a>
</p>

<p align="center">
  <a href="https://github.com/Torinomii/codex-hosts/releases">Download</a> · <a href="#installation">Installation</a> · <a href="#quick-start">Quick start</a> · <a href="#release-history">Release history</a> · <a href="#technical-reference">Technical reference</a>
</p>

A Windows SSH / Telnet host manager and MCP server for Codex. Save a host once, locally or in Bitwarden or 1Password, then ask Codex to connect, run commands or work across several machines. Passwords, key passphrases and FIDO PINs are entered locally and never reach the conversation or tool parameters.

<p align="center">
  <img src="Main.png" alt="Codex Hosts main window with local, Bitwarden and 1Password hosts" width="960" />
</p>

## At a glance

| Capability | What you can do |
| --- | --- |
| Host management | Save SSH / Telnet hosts with descriptions and tags; search, filter, import and export them. |
| Credential stores | Keep logins in Windows Credential Manager, a Bitwarden folder or a 1Password vault. |
| Authentication | Passwords, OpenSSH private keys, FIDO / YubiKey handles, Windows OpenSSH Agent or Pageant. |
| Remote commands | Run commands, send stdin, or run a program as an explicitly chosen user with sudo / su. |
| Parallel work | Run several commands over one SSH connection, or across an explicit list of hosts. |
| Connections | Verified host keys and jump chains; re-authenticate per call, keep the session, or drop it when idle. |
| Call approval | Optionally review every SSH call before it connects. |
| Temporary secrets | Keep API keys and tokens in memory only and inject them into a local program's environment. |

## What's new in 0.4.0

- **Bitwarden hosts:** every Login item in a chosen folder is a host (name = alias, `ssh://` or `telnet://` URI = endpoint, username and password = login). It is read through the official `bw` CLI before each use, so vault edits apply on the next call.
- **1Password hosts:** the same for the Login items of a chosen 1Password vault, read through the official `op` CLI and the 1Password desktop app. Unlocking stays in 1Password.
- **Managed in the window:** create, edit and delete vault hosts in the same editor as local ones. Jump hosts, call approval and Advanced settings stay in `codex-hosts`.

[Compare changes](https://github.com/Torinomii/codex-hosts/compare/v0.3.3...v0.4.0)

## Installation

The quickest way is to ask Codex. Paste this into Codex:

```text
Install the latest release of codex-hosts from https://github.com/Torinomii/codex-hosts/releases/latest: download codex-hosts-windows-x86_64.zip and codex-hosts-windows-x86_64.zip.sha256, and stop if the checksum does not match.
Ask me which directory to install it in, and tell me it must stay in place because the installed Skill links to it. Extract the ZIP there and run install-local-skill.ps1 -ReleasePackage; ask me before replacing an earlier installation.
Confirm that the installed codex-hosts Skill directory is one symbolic link to that directory, then register the codex-hosts MCP server in config.toml as described in SKILL.md.
```

Codex downloads and verifies the latest Release, installs it where you choose and registers the MCP server. Nothing is compiled. To install by hand, follow the two steps after the table.

| Route | Requirements | Entry point |
| --- | --- | --- |
| Ask Codex (recommended) | Codex + the GitHub Release requirements | The prompt above |
| GitHub Release | 64-bit Windows 10+ · PowerShell 7.2+ | `install.cmd` or the command below |
| Source checkout | The above + Rust 1.92+, MSVC and Windows SDK | `scripts/install.cmd` or `scripts/install-local-skill.ps1` |

### 1. Install the package

Download `codex-hosts-windows-x86_64.zip` from the [Releases page](https://github.com/Torinomii/codex-hosts/releases), check it against the `.sha256` file published next to it (`(Get-FileHash .\codex-hosts-windows-x86_64.zip).Hash`), extract it to a stable directory and run:

```powershell
pwsh -NoProfile -File .\install-local-skill.ps1 -ReleasePackage
```

Double-clicking `install.cmd` runs the same guided installer. It checks the environment and asks before replacing an earlier installation.

**Keep the extracted directory in place.** The installed Skill is a symbolic link to it, so Windows must allow the current user to create symbolic links.

### 2. Register the MCP server

Add this to `%USERPROFILE%\.codex\config.toml` (or a workspace `.codex\config.toml`), replacing `<user>` with your Windows account name:

```toml
[mcp_servers.codex-hosts]
command = "C:\\Users\\<user>\\.codex\\skills\\codex-hosts\\bin\\codex-hosts.exe"
args = ["--mcp"]
startup_timeout_sec = 20
tool_timeout_sec = 86400
```

<details>
<summary><strong>Build from source</strong></summary>

```powershell
git clone https://github.com/Torinomii/codex-hosts.git
cd codex-hosts
pwsh -NoProfile -File .\scripts\install-local-skill.ps1
```

The script checks PowerShell, Rust, the MSVC toolchain and the Windows SDK, explains anything missing, then asks before compiling and before installing links. It links the installed Skill to `skill\codex-hosts` and the Skill's `bin\codex-hosts.exe` to the build, so a rebuild updates the installed program. Replaced paths are kept as `.codex-hosts.backup-*`.

To build manually, run `cargo build --locked --release`, then install with `-SkipBuild`.

If symbolic links cannot be created, enable Developer Mode or use an elevated PowerShell; the script never changes system settings itself.

| Option | Effect |
| --- | --- |
| `-CodexHome <directory>` | Install under another Codex home instead of `CODEX_HOME` / `%USERPROFILE%\.codex` |
| `-NonInteractive -Build -Install` | Unattended build and install; add `-ReplaceExisting` to replace an old installation |
| `-SkipBuild` / `-SkipInstall` | Use an existing build / build only |

</details>

## Quick start

### 1. Add a host

Open `codex-hosts.exe`, choose **New host**, enter the alias, address, port, user, protocol and authentication, and save.

| Authentication | Uses |
| --- | --- |
| Password | A password kept in Windows Credential Manager, or in the Bitwarden / 1Password item |
| OpenSSH Key | An OpenSSH private-key file |
| FIDO / Security Key | An OpenSSH FIDO handle on a hardware key |
| SSH Agent | An identity loaded in Windows OpenSSH Agent or Pageant |

### 2. Optional: keep hosts in Bitwarden or 1Password

Open **Credential services** in the toolbar and choose **Start setup…**. Afterwards **New host** can also create hosts in that store.

| Store | Requirements | Hosts |
| --- | --- | --- |
| Bitwarden | The official Bitwarden CLI (`bw.exe`); an account on bitwarden.com, bitwarden.eu or a self-hosted server | Login items in one folder |
| 1Password | The 1Password desktop app with **Settings › Developer › Integrate with 1Password CLI** on, and the official 1Password CLI (`op.exe`) | Login items in one vault; a dedicated vault is recommended |

### 3. Use the host from Codex

Name the saved alias and the task:

```text
Connect to example and run hostname.
Check disk usage on web-1 and web-2.
```

The Skill looks up the host, verifies its key, authenticates and returns structured results; you do not call MCP tools yourself.

## Security boundaries

| Control | Behaviour |
| --- | --- |
| Login credentials | Kept in Windows Credential Manager or the host's Bitwarden / 1Password item; never in host profiles, tool parameters, command lines or results. FIDO PINs are never saved. |
| Password managers | The Bitwarden master password is typed only in `codex-hosts`; 1Password is unlocked only in the 1Password app. |
| Host identity | Unknown or changed SSH host keys need your confirmation, on every jump hop too. |
| Kept sessions | A kept session skips later authentication and key touches; call approval is separate. |
| Sudo / su | The execution user is chosen per call; saved-password supply needs its own permission. |
| Temporary secrets | Memory only and approved per field by default; cleared when the tray application exits. |

> **Telnet has no encryption.** Credentials, commands and output may cross the network in plaintext. Use it only on networks you trust; prefer SSH.

<details>
<summary><strong>Credential storage and password managers</strong></summary>

Passwords and key passphrases of local hosts are stored in Windows Credential Manager under the signed-in Windows account. A FIDO PIN is used for the current operation only.

The host store (`hosts.json`) must be owned by the Windows account running `codex-hosts`. Otherwise the MCP server refuses to start and logs the `icacls /setowner` command that repairs it; the window only warns.

A Bitwarden or 1Password host is its Login item: alias, endpoint, username, password, description and tags live in the item, the remaining settings in `codex-hosts`. Saving the host in the window writes the item; deleting it moves the Bitwarden item to the trash or archives the 1Password item, so both can be restored. An edit never overwrites an item that was changed in the vault in the meantime.

- **Bitwarden:** **Ask again** sets how long the vault stays unlocked: every call, until idle for a set time, until `codex-hosts` closes, or until you lock it. The kept session key is stored in Windows Credential Manager, and ending it also runs `bw lock`.
- **1Password:** `codex-hosts` never handles your account password. The 1Password app asks you to allow codex-hosts and decides how long that lasts.

</details>

<details>
<summary><strong>Host keys and authentication persistence</strong></summary>

On the first connection `codex-hosts` shows the server's host-key fingerprint and saves it only after you confirm it. A changed key is never replaced automatically.

Each host chooses in its **Authentication** card how long Codex keeps the authenticated session:

| Option | Behaviour |
| --- | --- |
| Re-authenticate on every operation | The connection closes when the call ends. Hosts from earlier versions and imported hosts use this. |
| Keep for the Codex session | Proposed for new hosts. The session stays open until Codex exits, the link drops or `disconnect` is called. |
| Disconnect after idle time | The session closes after the chosen number of idle minutes. |

While a session is kept, later commands on that host run without authenticating or touching the security key again. The editor asks you to confirm this choice when you first save it and whenever you change it, and such hosts are marked in the list. Telnet hosts always re-authenticate.

</details>

<details>
<summary><strong>Temporary secrets</strong></summary>

`codex-hosts` can hold API keys, tokens and other values unrelated to host login. They live only in the `codex-hosts` process memory, are never returned to Codex, and are injected into the environment of a local program that Codex runs.

Every field requires approval by default: before a program that uses it starts, a dialog asks you. Turn approval off per field to let runs start directly. Values expire when the tray application exits, you sign out or Windows restarts; Codex restarting does not clear them.

See [`temporary-secrets.md`](skill/codex-hosts/references/temporary-secrets.md).

</details>

## Release history

<details>
<summary><strong>0.4.0 · Bitwarden and 1Password hosts</strong></summary>

- **Bitwarden hosts:** the Login items of one folder are hosts; description and tags are stored in `codex-hosts:` custom fields. Each use re-reads the item through the official `bw` CLI after a sync no older than the chosen window, and a host created in the folder is found by alias right away.
- **Bitwarden unlock:** choose how long the vault stays unlocked (every call, idle time, until exit, until locked). Supports bitwarden.com, bitwarden.eu, self-hosted servers and new-device verification.
- **1Password hosts:** the Login items of one vault are hosts, read through the official `op` CLI and the 1Password desktop app. Setup finds `op.exe` and can create the vault.
- **Editing:** vault hosts are created, edited and deleted in the same editor as local hosts; a concurrent change in the vault is reported instead of overwritten.
- **Codex:** `list_hosts` reports `credential_source`, and new `BITWARDEN_*` / `ONEPASSWORD_*` error codes say what the user has to do. Sudo and su reuse the password read from the item.

[Compare changes](https://github.com/Torinomii/codex-hosts/compare/v0.3.3...v0.4.0)

</details>

<details>
<summary><strong>0.3.3 · Sudo / su, call approval and guided installation</strong></summary>

- **Sudo / su:** `exec_sudo` and `exec_su` run an absolute program with exact arguments as an explicitly chosen user. Separate Advanced switches allow supplying saved passwords; passwordless execution works with them off.
- **Call approval:** an optional per-host switch reviews each SSH call, including probes, user switches and batches, before it connects. Kept connections never reuse an approval.
- **Temporary secrets:** each field has its own approval switch, on by default. Saving or clearing fields cancels waiting runs.
- **Credential safety:** saved passwords stay bound to their accounts; changing a login never hands another account's password over.
- **Installation:** Release and source installs use symbolic links only, through a step-by-step script with environment checks and backups.

[Compare changes](https://github.com/Torinomii/codex-hosts/compare/v0.3.2...v0.3.3)

</details>

<details>
<summary><strong>0.3.2 · Complete Skill directory linking</strong></summary>

- The installed `codex-hosts` Skill links to the whole source Skill directory, keeping `SKILL.md`, `agents` and `references` together.

[Compare changes](https://github.com/Torinomii/codex-hosts/compare/v0.3.1...v0.3.2)

</details>

<details>
<summary><strong>0.3.1 · MCP tools, session persistence and connection reliability</strong></summary>

- **MCP server:** `codex-hosts.exe --mcp` serves Codex over stdio, with risk annotations on every tool. The older file-based tool mode keeps working.
- **Authentication persistence:** each host re-authenticates per call, keeps the session for the Codex session, or drops it after an idle time.
- **Editor:** required fields are marked, and an **Advanced** card holds per-host tuning (channels, timeouts, keepalive, remote environment, hiding from Codex, Telnet prompts).
- Cancelling a Codex call closes the channel; long work belongs in the remote host's `tmux` / `screen`.
- More reliable Telnet logins, jump-hop cleanup and idle timing; Releases carry a SHA-256 checksum.

[Compare changes](https://github.com/Torinomii/codex-hosts/compare/v0.3.0...v0.3.1)

</details>

<details>
<summary><strong>0.3.0 · Host manager redesign, descriptions and tags</strong></summary>

- **Descriptions and tags:** search and filter by notes or tags; `list_hosts` returns them and filters by `tags`, and `exec` accepts `stdin`.
- **Redesigned window:** resizable host list, grouped host details, status bar and notices, batch selection, and light and dark themes.

[Compare changes](https://github.com/Torinomii/codex-hosts/compare/v0.2.3...v0.3.0)

</details>

## Technical reference

<details>
<summary><strong>MCP tools</strong></summary>

Every tool returns the same JSON as `structuredContent` and as text, and carries MCP annotations that tell read-only tools from ones that execute code.

| Tool | Purpose |
| --- | --- |
| `list_hosts` | Saved hosts with non-secret details, `credential_source`, trust state, `auth_persistence`, `require_call_approval`, sudo/su permissions, `max_channels`, `remote_env` |
| `agent_identities`, `fido_identities` | Public details of loaded Agent keys and FIDO handles |
| `probe`, `batch_probe` | Authenticate and run `hostname`; report unknown or changed host keys |
| `exec`, `exec_stdin`, `exec_many`, `batch_exec` | Run commands; `exec_stdin` is separate because stdin content cannot be reviewed like a command |
| `exec_sudo`, `exec_su` | Run an absolute program with exact arguments as the `user` given on each call |
| `disconnect` | Drop kept sessions |
| `open_host_editor` | Open the editor prefilled with non-secret details, or to confirm a reported host key |
| `temporary_secrets_open`, `_status`, `_run`, `_clear` | Memory-only secrets held by the tray application |

Parameters never contain a password, passphrase or PIN. Timeouts (`connect_timeout_ms`, `command_timeout_ms`, `batch_timeout_ms`) are per call and default to 120 s to connect and 10 minutes per command, or to the host's Advanced defaults. `open_host_editor` accepts `password`, `private-key` (also for FIDO handles) and `ssh-agent` as authentication.

**Approve each Codex call** (Advanced, off by default) shows the call's hosts and commands before connecting. One approval covers one call, all its batch hosts included; the next call asks again, even on a kept connection. Unanswered reviews expire after five minutes. See [approvals.md](skill/codex-hosts/references/approvals.md).

**Sudo / su:** Codex must name the execution user on every call (`"user":"root"` for root). **Allow saved passwords for sudo** and **Allow saved passwords for su** in Advanced separately allow supplying a saved password: sudo reuses the SSH login password or a separate one, su uses the target account's password. With a switch off, passwordless paths still work and a password prompt fails with `SUDO_AUTO_AUTH_DISABLED` or `SU_AUTO_AUTH_DISABLED`. Only the outer authentication is handled; sudo inside a script never receives a password. See [sudo.md](skill/codex-hosts/references/sudo.md) and [su.md](skill/codex-hosts/references/su.md) for the remote requirements.

See [`SKILL.md`](skill/codex-hosts/SKILL.md) for the full Codex behaviour and safety rules.

</details>

<details>
<summary><strong>Multiple commands and hosts</strong></summary>

`exec_many` runs several short commands concurrently over one SSH connection, so a security key is touched once:

```json
{"alias":"example","commands":["hostname","uptime","df -h"],"max_concurrency":8}
```

`batch_exec` and `batch_probe` take an explicit host list; an empty list never means all hosts:

```json
{"aliases":["web-1","web-2","web-3"],"command":"uptime","max_concurrency":8,"batch_timeout_ms":30000}
```

`exec_stdin` sends exact UTF-8 text (up to 1 MiB) to one SSH host's command, then EOF:

```json
{"alias":"example","command":"python3 -","stdin":"print('hello')\n","command_timeout_ms":10000}
```

</details>

<details>
<summary><strong>Descriptions, tags and CSV</strong></summary>

Hosts store a multiline `description` and any number of `tags`. Search matches alias, address, username, notes and tags; several tag filters must all match. `list_hosts` returns both fields and accepts a `tags` filter, and `open_host_editor` can prefill them.

CSV import and export include optional `description` and `tags` columns; the tags cell is a JSON array such as `["prod","web"]`.

</details>

<details>
<summary><strong>Limits, timeouts and long-running work</strong></summary>

- One command captures at most **1 MiB** of output; an `exec_many` or batch result at most **8 MiB**. Check `output_truncated`.
- `tool_timeout_sec` is only a ceiling; each call carries its own, shorter timeout.
- Commands are never retried automatically, because a remote command may not be safe to run twice.
- Cancelling a Codex call closes the channel and returns `CANCELLED`; a command that already started may keep running.
- Work longer than a few minutes belongs in the remote host's `tmux` / `screen`; see [`long-running.md`](skill/codex-hosts/references/long-running.md).

`probe` and `batch_probe` are marked read-only but still authenticate, so a security key may need a touch. To have Codex confirm them too:

```toml
[mcp_servers.codex-hosts.tools.probe]
approval_mode = "prompt"

[mcp_servers.codex-hosts.tools.batch_probe]
approval_mode = "prompt"
```

</details>

<details>
<summary><strong>FIDO / security keys, SSH Agent and jump hosts</strong></summary>

- **FIDO:** existing OpenSSH ECDSA-SK and Ed25519-SK handles (`id_ecdsa_sk`, `id_ed25519_sk`) work without an agent; the private key stays on the device, and a PIN or touch may be required. Credentials created in the app use ECDSA-SK.
- **SSH Agent:** authentication is delegated to an already running Windows OpenSSH Agent or Pageant; `codex-hosts` never starts or configures an agent.
- **Jump hosts:** an SSH host can go through other saved, verified SSH hosts, up to 8 in a chain. Every hop verifies its host key, loops are rejected, and agent forwarding is always off.

</details>

## License

This project is licensed under the [Apache License 2.0](LICENSE).
