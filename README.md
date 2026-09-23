# codex-hosts

<p align="center">
  <a href="https://github.com/Torinomii/codex-hosts/actions/workflows/release.yml"><img src="https://github.com/Torinomii/codex-hosts/actions/workflows/release.yml/badge.svg" alt="Build release" /></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/License-Apache--2.0-D22128.svg" alt="Apache 2.0 License" /></a>
  <img src="https://img.shields.io/badge/Windows-10%2B-0078D4?logo=windows&logoColor=white" alt="Windows 10 or newer" />
  <img src="https://img.shields.io/badge/Rust-1.92%2B-000000?logo=rust&logoColor=white" alt="Rust 1.92 or newer" />
  <a href="https://linux.do/"><img src="https://img.shields.io/badge/LINUX-DO-FFB003" alt="LINUX DO Community" /></a>
</p>

[English](README.md) | [简体中文](docs/readme/README_zh-CN.md) | [繁體中文](docs/readme/README_zh-TW.md) | [日本語](docs/readme/README_ja.md)

`codex-hosts` is a Windows SSH / Telnet host manager for Codex. It runs as an MCP server, so Codex connects to and operates remote hosts through structured tools without ever handling sensitive credentials such as passwords, private-key passphrases, or FIDO PINs in chat, command arguments, or files.

![Codex Hosts main window](Main.png)

## Features

- Manage reusable SSH / Telnet host profiles.
- Password and ordinary OpenSSH private-key authentication.
- FIDO / YubiKey hardware-backed SSH keys.
- Identities already loaded in Windows OpenSSH Agent or Pageant.
- SSH host-key pinning with explicit user confirmation.
- Verified SSH jump-host chains.
- Single-host command execution.
- Concurrent commands over one SSH connection.
- Multi-host SSH / Telnet probing and execution.
- Memory-only temporary secrets for API keys, tokens, and other values that should not be exposed to Codex.
- An MCP server with risk-annotated tools, plus a Codex Skill for host lookup, connection, authentication, and command execution.
- Per-host authentication persistence: keep the session while Codex runs (proposed for new hosts and confirmed on the first save), re-authenticate on every operation, or drop it after idle time.

## What's new in 0.3.1

- MCP server: `codex-hosts.exe --mcp` serves Codex over stdio. Hosts, probes, commands, batches, the host editor and temporary secrets are MCP tools with risk annotations; no shell, PowerShell wrapper or temporary request files are involved any more. The previous file-based tool mode keeps working unchanged for Skills that have not been updated.
- Authentication persistence: every host now chooses whether Codex keeps the session for the Codex session (proposed for new hosts; the choice is confirmed on the first save and whenever it changes), re-authenticates on every operation (the previous behaviour, kept for hosts from earlier versions and for imports), or drops it after a chosen idle time. Hosts that keep sessions are marked in the list and carry a warning in the editor.
- Editor: required fields are marked with `*` and highlighted when a save is attempted with one empty; a new **Advanced** card holds per-host tuning (concurrent channels, default timeouts, keepalive interval, a remote-environment hint for Codex, hiding a host from Codex, and Telnet prompt overrides).
- Cancelling a Codex call now stops the wait and closes the channel; long-running work is documented around the remote host's own `tmux` / `screen` instead of long waits.
- `codex-hosts.exe --version` prints the version, and argument errors are reported on stderr. The executable is a windowed program, so both are visible only when captured (`codex-hosts.exe --version | more`).

## Installation

### Download a release

The prebuilt version supports 64-bit Windows 10 or newer. Download it from [Releases](https://github.com/Torinomii/codex-hosts/releases/latest).

### Install the Codex Skill

The installed Skill should look like this:

```text
%USERPROFILE%\.codex\skills\codex-hosts\
├── SKILL.md
├── bin\
│   └── codex-hosts.exe
├── agents\
└── references\
```

Extract the release ZIP to a directory you will keep, then run its link installer from that directory:

```powershell
pwsh -NoProfile -File .\install-local-skill.ps1 -ReleasePackage
```

The installer links the complete Skill and `bin\codex-hosts.exe` to the extracted release. Keep that directory in place; moving or deleting it breaks the links. Windows must permit symbolic-link creation for the current user. Then register the MCP server in `%USERPROFILE%\.codex\config.toml` (or a workspace `.codex\config.toml`):

```toml
[mcp_servers.codex-hosts]
command = "C:\\Users\\<user>\\.codex\\skills\\codex-hosts\\bin\\codex-hosts.exe"
args = ["--mcp"]
startup_timeout_sec = 20
tool_timeout_sec = 86400
```

`tool_timeout_sec` is only a ceiling; each call carries its own timeout. If you never use blocking waits for long jobs you can leave Codex's default.

Codex runs `probe` and `batch_probe` without asking, because they are marked read-only: they only run `hostname`, and letting them run unattended (and in parallel) is what makes pre-authenticating a host that keeps its session useful. They still connect and authenticate, so a hardware key will ask for a touch. If you would rather confirm each one, tell Codex so:

```toml
[mcp_servers.codex-hosts.tools.probe]
approval_mode = "prompt"

[mcp_servers.codex-hosts.tools.batch_probe]
approval_mode = "prompt"
```

Do not install only `SKILL.md` or the executable; keep the complete extracted release.

You can also ask Codex to install it:

```text
Download the latest codex-hosts release from https://github.com/Torinomii/codex-hosts/releases/latest, extract it to a stable directory, and run its install-local-skill.ps1 with -ReleasePackage.
Confirm that the complete Skill and executable are symbolic links, register the codex-hosts MCP server in config.toml as described in SKILL.md, and keep the extracted directory in place.
```

### Build from source

Source builds require Rust 1.92 or newer and the MSVC toolchain:

```powershell
git clone https://github.com/Torinomii/codex-hosts.git
cd codex-hosts
cargo build --locked --release
```

The executable is created at:

```text
target\release\codex-hosts.exe
```

After the build completes, run the linked installation command below.

### Link a local source checkout

When developing from a stable local checkout, install symbolic links instead of copying after every build:

```powershell
pwsh -NoProfile -File .\scripts\install-local-skill.ps1
```

The script links `SKILL.md`, `agents`, and `references` to `skill\codex-hosts`, and links the installed `bin\codex-hosts.exe` directly to `target\release\codex-hosts.exe`. It validates all sources before replacing an existing installation, verifies the final targets, and rolls back if installation fails. Run it again at any time to verify the same layout. Windows must permit symbolic-link creation for the current user.

The same installer also supports a retained, extracted release with `-ReleasePackage`; its ZIP includes the installer.

## Quick start

### 1. Add a host

Open `codex-hosts.exe` and create a host.

Enter:

- Alias
- Address or IP
- Port
- User name
- Protocol
- Authentication method
- Description and tags (optional)

Then save the profile.

### 2. Choose an authentication method

| Method | Description |
| --- | --- |
| Password | Password authentication, stored in Windows Credential Manager |
| OpenSSH Key | Ordinary OpenSSH private-key file |
| FIDO / Security Key | OpenSSH FIDO handle backed by a hardware device |
| SSH Agent | Identity already loaded in Windows OpenSSH Agent or Pageant |

Sensitive authentication values are not passed through the Codex conversation.

### 3. Use the host from Codex

After saving a host, tell Codex the host alias and the task to perform.

For example:

```text
Connect to example and run hostname.
```

Or:

```text
Check disk usage on web-1 and web-2.
```

You can also request several operations at once:

```text
Connect to server1 and check hostname, uptime, and disk space.
```

The Codex Skill handles:

- Host lookup
- Connection setup
- SSH host-key verification
- Authentication
- Command execution
- Structured results

Normal use does not require calling the tools manually.

## Security boundaries

### Login credentials

Passwords and private-key passphrases are persistently stored in Windows Credential Manager.

These sensitive values are not:

- Written into host profiles
- Placed in MCP tool parameters or results
- Passed as command-line arguments
- Returned to Codex

A FIDO PIN is used only for the current operation and is not saved.

The host store (`hosts.json`) must be owned by the Windows account running `codex-hosts`. The MCP server refuses to start when another account owns it and prints the `icacls /setowner` command that repairs it to stderr (visible in Codex's MCP log); the GUI only warns. This catches a store copied from another account, not an attacker who already runs as you.

### SSH host keys

SSH host fingerprints require explicit user confirmation.

On a first connection to an unknown host, `codex-hosts` displays the detected host fingerprint. It is saved only after the user confirms it.

If the server host key later changes, the saved fingerprint is not replaced automatically and must be confirmed again.

### Authentication persistence

Hosts from earlier versions and imported hosts re-authenticate on every Codex call, so a hardware key is touched once per action and Codex never holds an open session between calls. A new host is proposed with **Keep for the Codex session**; the editor asks you to confirm the choice on the first save and again whenever you change it, in either direction. Each host can pick any option in its **Authentication** card:

| Option | Behaviour |
| --- | --- |
| Re-authenticate on every operation | The connection closes when the call ends. Hosts from earlier versions and imports start here. |
| Keep for the Codex session | Proposed for new hosts. The authenticated session stays open, with keepalives, until Codex exits, the link drops, or `disconnect` is called. |
| Disconnect after idle time | The session stays open until it has been idle for the chosen number of minutes. |

While a session is kept, later Codex commands on that host run without re-authenticating or touching the security key, which removes the one-confirmation-per-action protection. The editor shows this warning, hosts that keep sessions carry a marker in the list, and Codex is told never to suggest changing the setting. Telnet hosts always re-authenticate. Credentials, PINs and host-key checks are unaffected; only the live session is reused.

### Temporary secrets

`codex-hosts` can also hold temporary API keys, tokens, and other sensitive values unrelated to host login.

These values live only in the current `codex-hosts` process memory. They are not stored in Windows Credential Manager and are not returned to Codex.

After user approval, they can be injected directly into the environment of a selected program.

They expire when the `codex-hosts` tray application exits, the user signs out, or the system restarts. Codex starting or stopping does not affect them.

See [`temporary-secrets.md`](skill/codex-hosts/references/temporary-secrets.md) for the complete behavior.

### Policy gates such as hol-guard

Because every action is an MCP tool call with structured parameters, a policy layer that intercepts `mcp__*` calls sees the tool name and its arguments. With [hol-guard](https://github.com/hashgraph-online/hol-guard), add the executable as a custom extension (`codex-hosts.exe --mcp`) and treat `exec`, `exec_stdin`, `exec_many`, `batch_exec`, `temporary_secrets_run`, and `open_host_editor` as tools to review; the remaining tools are read-only or only tighten access.

## FIDO / security keys

`codex-hosts` can use existing OpenSSH ECDSA-SK and Ed25519-SK FIDO handles.

For example:

```text
id_ecdsa_sk
id_ed25519_sk
```

New or recovered FIDO SSH credentials created through the application use ECDSA-SK.

In FIDO mode:

- Hardware private keys stay on the security device.
- The FIDO PIN is used only for the current operation and is not saved.
- SSH Agent is not required.
- Agent forwarding is always disabled.
- Authentication may require a PIN or touch, depending on the device configuration.

FIDO handles and SSH Agent are separate authentication paths.

FIDO handle:

```text
codex-hosts
    │
    ▼
OpenSSH FIDO Handle
    │
    ▼
Security key
```

`codex-hosts` performs hardware signing directly through system components.

SSH Agent:

```text
codex-hosts
    │
    ▼
OpenSSH Agent / Pageant
    │
    ▼
Loaded identity
```

Authentication is delegated to an already running Agent.

`codex-hosts` does not start, enable, or persist an Agent service automatically.

## Jump hosts

An SSH host can use another saved and verified SSH host as a jump host.

For example:

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

Rules:

- A jump host must be a verified SSH host.
- Every hop verifies its host key.
- Jump-host loops are rejected.
- An SSH chain can contain at most 8 hosts.
- Agent forwarding is always disabled.

## Telnet

`codex-hosts` also supports Telnet.

Telnet does not provide encryption, so user names, passwords, commands, and returned data may travel across the network in plaintext.

Use Telnet only on trusted networks where you explicitly accept that risk. SSH is recommended for Internet-facing connections.

## Batch execution

### Multiple commands on one SSH host

`exec_many` reuses one SSH connection to run several independent short commands concurrently:

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

It authenticates once and then opens multiple channels over the same SSH connection.

This is especially useful with FIDO / security keys because a group of commands can normally share one authentication.

`exec_many` is available only for SSH hosts.

### Multiple hosts

Specify the hosts explicitly:

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

Hosts can also be probed in a batch:

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

Batch mode requires an explicit host list. An empty list is never interpreted as all hosts.

<details>
<summary>MCP tools</summary>

Codex talks to `codex-hosts.exe --mcp` over stdio. Every tool returns the same JSON as `structuredContent` and as text, and carries MCP annotations so a policy layer can tell read-only tools from ones that execute code.

| Tool | Purpose |
| --- | --- |
| `list_hosts` | Saved hosts with non-secret details, trust state, `auth_persistence`, `max_channels`, `remote_env` |
| `agent_identities`, `fido_identities` | Public identity details of loaded Agent keys and FIDO handles |
| `probe`, `batch_probe` | Authenticate and run `hostname`; surface unknown or changed host keys |
| `exec`, `exec_stdin`, `exec_many`, `batch_exec` | Run commands; `exec_stdin` is separate because program text on stdin is opaque to review |
| `disconnect` | Drop sessions kept for hosts that opted in |
| `open_host_editor` | Open the editor window prefilled with non-secret details, or to confirm a reported host key |
| `temporary_secrets_open`, `_status`, `_run`, `_clear` | Memory-only secrets held by the tray application |

Parameters never contain a password, passphrase, or PIN; the editor collects those in masked fields. Timeouts (`connect_timeout_ms`, `command_timeout_ms`, `batch_timeout_ms`) are per call and default to 120 s to connect and 10 minutes per command, or to the host's own defaults from its Advanced card.

Authentication arguments to `open_host_editor` use stable names: `password`; `private-key` / `private_key` for an ordinary private-key file or a FIDO handle (`fido-handle` is also accepted); `ssh-agent` / `ssh_agent` for Windows OpenSSH Agent or Pageant.

When running remote commands, check `output_truncated` to see whether output was shortened by the size limit.

See [`SKILL.md`](skill/codex-hosts/SKILL.md) for the full Codex behavior, workflow, and safety rules.

</details>

## Host descriptions, tags and command input

Hosts can store a multiline `description` and multiple `tags`. Add/remove tags in the editor; surrounding whitespace, empty tags and case-insensitive duplicates are removed, preserving the first spelling. Saving only metadata preserves connection verification and host-key trust. Search matches alias, address, username, notes and tags; selecting multiple tag filters requires all of them. Batch select-all applies only to visible hosts, and changing filters drops hidden selections.

`list_hosts` includes both fields and accepts an optional `tags` array. Missing/empty filters return all hosts; unknown tags return no matches. `open_host_editor` prefills `description` and `tags`. Omitted fields preserve saved metadata; an empty description or a sole empty tag clears that field. CSV templates/import/export include optional `description` and `tags` columns; the tags cell is a JSON array such as `["prod","web"]` (CSV quoting applies). Legacy stores and CSV files remain supported.

```json
{"tags":["prod","web"]}
{"alias":"example","command":"python3 -","stdin":"print('hello')\n","command_timeout_ms":10000}
```

`exec_stdin` sends exact UTF-8 text, up to 1 MiB, to a single SSH host's command without adding a newline, then EOF; `""` sends EOF immediately. Input and output progress concurrently under the existing deadlines and output limits. A remote program can still exit before consuming all input; its exit status remains authoritative. Oversized input returns `STDIN_TOO_LARGE`; Telnet hosts return `STDIN_UNSUPPORTED`. No command is automatically replayed.

## Execution limits

Command execution is deliberately bounded to prevent excessive output or memory use.

- A single command can capture at most **1 MiB** of output.
- A complete `exec_many` or batch JSON result is limited to **8 MiB**.
- `exec_many` and batch operations use bounded concurrency.
- Check `output_truncated` when output may have been shortened.
- Remote commands are not automatically retried after network failures.
- Cancelling a Codex call closes the channel and returns `CANCELLED`; a remote command that already started may keep running.
- Work that takes more than a few minutes belongs in the remote host's `tmux` / `screen`; see [`long-running.md`](skill/codex-hosts/references/long-running.md).

Automatic retries are avoided because remote commands may not be idempotent, for example:

```text
reboot
rm
systemctl restart
database writes
deployment operations
```

A network failure does not mean that running the same command again is safe.

## Project structure

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

This project is licensed under the [Apache License 2.0](LICENSE).
