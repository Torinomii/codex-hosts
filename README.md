# codex-hosts

<p align="center">
  <a href="https://github.com/Torinomii/codex-hosts/actions/workflows/release.yml"><img src="https://github.com/Torinomii/codex-hosts/actions/workflows/release.yml/badge.svg" alt="Build release" /></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/License-Apache--2.0-D22128.svg" alt="Apache 2.0 License" /></a>
  <img src="https://img.shields.io/badge/Windows-10%2B-0078D4?logo=windows&logoColor=white" alt="Windows 10 or newer" />
  <img src="https://img.shields.io/badge/Rust-1.92%2B-000000?logo=rust&logoColor=white" alt="Rust 1.92 or newer" />
  <a href="https://linux.do/"><img src="https://img.shields.io/badge/LINUX-DO-FFB003" alt="LINUX DO Community" /></a>
</p>

[English](README.md) | [简体中文](docs/readme/README_zh-CN.md) | [繁體中文](docs/readme/README_zh-TW.md) | [日本語](docs/readme/README_ja.md)

`codex-hosts` is a Windows SSH / Telnet host manager for Codex. It lets Codex connect to and operate remote hosts without directly handling sensitive credentials such as passwords, private-key passphrases, or FIDO PINs in chat, command arguments, or request files.

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
- A complete Codex Skill for host lookup, connection, authentication, and command execution.

## What's new in 0.3.0

- Host descriptions and tags: search and filter the list by notes or tags, and let Codex prefill them (`--description`, `--tag`). `list_hosts` returns both fields and accepts a `tags` filter, and `exec` accepts `stdin` for command input.
- Redesigned window: a resizable host list with a compact header, host details grouped into Basic information / Connection / Authentication / SSH host key cards, and a fixed action bar for Test connection and Save.
- Status bar and notifications: every outcome is shown in the bottom status bar, and warnings or errors also appear as a dismissible notice in the top-right corner.
- Batch management: a selection bar with the selected count, Select all, Export and Delete appears below the toolbar; rows get checkboxes and `Esc` leaves the mode.
- Import and export now open as dialogs that must be closed before continuing; all confirmation dialogs share one layout, with destructive actions highlighted.
- Layout adapts to the window size: the form uses two columns from 1040 px wide and stacks labels above fields at the minimum window size; light and dark Windows themes are both supported.

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

For a manual installation:

1. Place the release executable at:

```text
%USERPROFILE%\.codex\skills\codex-hosts\bin\codex-hosts.exe
```

2. Copy the complete contents of `skill\codex-hosts` from the release into:

```text
%USERPROFILE%\.codex\skills\codex-hosts
```

Do not install only `SKILL.md` or the executable; keep the complete Skill directory.

You can also ask Codex to install it:

```text
Download and install the latest codex-hosts release from https://github.com/Torinomii/codex-hosts/releases/latest.
Automatically find the Skill installation directory for the current environment, install the complete Skill and executable, and confirm that all required files are in place.
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

Place it in the Skill's `bin` directory after the build completes.

### Link a local source checkout

When developing from a stable local checkout, install symbolic links instead of copying after every build:

```powershell
pwsh -NoProfile -File .\scripts\install-local-skill.ps1
```

The script links `SKILL.md`, `agents`, and `references` to `skill\codex-hosts`, and links the installed `bin\codex-hosts.exe` directly to `target\release\codex-hosts.exe`. It validates all sources before replacing an existing installation, verifies the final targets, and rolls back if installation fails. Run it again at any time to verify the same layout. Windows must permit symbolic-link creation for the current user.

This linked workflow is for a local source checkout. Keep using the copy-based steps above for downloaded release archives.

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

Normal use does not require writing Tool JSON manually.

## Security boundaries

### Login credentials

Passwords and private-key passphrases are persistently stored in Windows Credential Manager.

These sensitive values are not:

- Written into host profiles
- Placed in Tool JSON
- Passed as command-line arguments
- Returned to Codex

A FIDO PIN is used only for the current operation and is not saved.

### SSH host keys

SSH host fingerprints require explicit user confirmation.

On a first connection to an unknown host, `codex-hosts` displays the detected host fingerprint. It is saved only after the user confirms it.

If the server host key later changes, the saved fingerprint is not replaced automatically and must be confirmed again.

### Temporary secrets

`codex-hosts` can also hold temporary API keys, tokens, and other sensitive values unrelated to host login.

These values live only in the current `codex-hosts` process memory. They are not stored in Windows Credential Manager and are not returned to Codex.

After user approval, they can be injected directly into the environment of a selected program.

They expire when `codex-hosts` exits, the user signs out, or the system restarts.

See [`temporary-secrets.md`](skill/codex-hosts/references/temporary-secrets.md) for the complete behavior.

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

It authenticates once and then opens multiple channels over the same SSH connection.

This is especially useful with FIDO / security keys because a group of commands can normally share one authentication.

`exec_many` is available only for SSH hosts.

### Multiple hosts

Specify the hosts explicitly:

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

Hosts can also be probed in a batch:

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

Batch mode requires an explicit host list. An empty list is never interpreted as all hosts.

<details>
<summary>Codex / Tool interface</summary>

### GUI edit mode

Codex or a script can open the host editor with non-secret connection details prefilled:

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

Do not pass passwords, private-key passphrases, or FIDO PINs as arguments.

Authentication arguments use stable names:

- `password`: password authentication.
- `private-key` / `private_key`: ordinary private-key file or FIDO handle; `fido-handle` is also accepted.
- `ssh-agent` / `ssh_agent`: Windows OpenSSH Agent or Pageant.

`private-key` covers both ordinary OpenSSH private keys and FIDO handles.

`ssh-agent` refers specifically to Agent / Pageant mode and not to every hardware-key authentication path.

### Tool mode

Tool mode uses UTF-8 JSON request and result files. Neither file may contain credentials.

Common requests:

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

`agent_identities` and `fido_identities` return only public identity details and public keys.

When running remote commands, check `output_truncated` to see whether output was shortened by the size limit.

See [`SKILL.md`](skill/codex-hosts/SKILL.md) for the full Codex behavior, workflow, and safety rules.

</details>

## Host descriptions, tags and command input

Hosts can store a multiline `description` and multiple `tags`. Add/remove tags in the editor; surrounding whitespace, empty tags and case-insensitive duplicates are removed, preserving the first spelling. Saving only metadata preserves connection verification and host-key trust. Search matches alias, address, username, notes and tags; selecting multiple tag filters requires all of them. Batch select-all applies only to visible hosts, and changing filters drops hidden selections.

`list_hosts` includes both fields and accepts an optional `tags` array. Missing/empty filters return all hosts; unknown tags return no matches. Editor prefill supports `--description "notes"` and repeated `--tag prod --tag web`. Omitted fields preserve saved metadata; an empty description or a sole empty tag clears that field. CSV templates/import/export include optional `description` and `tags` columns; the tags cell is a JSON array such as `["prod","web"]` (CSV quoting applies). Legacy stores and CSV files remain supported.

```json
{"action":"list_hosts","tags":["prod","web"]}
{"action":"exec","alias":"example","command":"python3 -","stdin":"print('hello')\n","command_timeout_ms":10000}
```

Single-host SSH `exec` accepts optional UTF-8 `stdin`, up to 1 MiB. The client sends exact bytes without adding a newline, then EOF. Missing or `null` input keeps previous behavior; `""` sends EOF immediately. Input and output progress concurrently under the existing deadlines and output limits. A remote program can still exit before consuming all input; its exit status remains authoritative.

`capabilities` advertises `exec_stdin`, `exec_stdin_protocols`, `max_stdin_bytes`, `host_metadata_fields` and `list_hosts_tag_filter`. Oversized input returns `STDIN_TOO_LARGE`; provided stdin for Telnet, `exec_many` or `batch_exec` returns `STDIN_UNSUPPORTED`. No command is automatically replayed.

## Execution limits

Command execution is deliberately bounded to prevent excessive output or memory use.

- A single command can capture at most **1 MiB** of output.
- A complete `exec_many` or batch JSON result is limited to **8 MiB**.
- `exec_many` and batch operations use bounded concurrency.
- Check `output_truncated` when output may have been shortened.
- Remote commands are not automatically retried after network failures.

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
