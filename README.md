# codex-hosts

<p align="center">
  <a href="https://github.com/Torinomii/codex-hosts/actions/workflows/release.yml"><img src="https://github.com/Torinomii/codex-hosts/actions/workflows/release.yml/badge.svg" alt="Build release" /></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/License-Apache--2.0-D22128.svg" alt="Apache 2.0 License" /></a>
  <img src="https://img.shields.io/badge/Windows-10%2B-0078D4?logo=windows&logoColor=white" alt="Windows 10 or newer" />
  <img src="https://img.shields.io/badge/Rust-1.92%2B-000000?logo=rust&logoColor=white" alt="Rust 1.92 or newer" />
  <a href="https://linux.do/"><img src="https://img.shields.io/badge/LINUX-DO-FFB003" alt="LINUX DO Community" /></a>
</p>

[English](README.md) | [简体中文](docs/readme/README_zh-CN.md) | [繁體中文](docs/readme/README_zh-TW.md) | [日本語](docs/readme/README_ja.md)

`codex-hosts` is a Windows SSH / Telnet host manager for Codex, allowing Codex to initiate connections securely without accessing passwords or private keys.

![Codex Hosts main window](Main.png)

## Highlights

- Save an alias, address, port, user name, protocol, authentication method, optional private-key path, trusted SSH host key, and optional jump host.
- Download a CSV template and import multiple host profiles without overwriting existing aliases or credentials; extra spreadsheet columns are ignored.
- Test every saved host at once with a 10-second limit per host and clear success or failure colors.
- Select multiple hosts for batch deletion while preserving jump hosts that are still in use.
- Use SSH password and keyboard-interactive authentication, or an OpenSSH private key with an optional passphrase.
- Store passwords and private-key passphrases only in Windows Credential Manager.
- Build multi-hop SSH chains from saved, verified SSH hosts.
- Let Codex pre-create an alias-specific draft and wait for Save, Trust, or Cancel without asking for credentials in chat.

## Installation

### Download a Release

The prebuilt package supports 64-bit Windows 10 or newer and can be downloaded from [Releases](https://github.com/Torinomii/codex-hosts/releases/latest).

### Install

Copy `bin\codex-hosts.exe` to `skill\codex-hosts\bin\codex-hosts.exe`, then copy the `skill\codex-hosts` directory to `%USERPROFILE%\.codex\skills\codex-hosts`.

### Install with a Codex prompt

```text
Download the latest codex-hosts release from https://github.com/Torinomii/codex-hosts/releases/latest and install it: automatically identify the Skill installation directory in the current environment, install the complete codex-hosts Skill and executable in the correct location, and confirm that the Skill, executable, and required configuration files exist.
```

### Build from source

Source builds require Rust 1.92 or newer with the MSVC toolchain:

```
git clone https://github.com/Torinomii/codex-hosts.git
cd codex-hosts
cargo build --locked --release
```

Copy `target\release\codex-hosts.exe` to `skill\codex-hosts\bin\codex-hosts.exe`, then install the complete `skill\codex-hosts` directory as `%USERPROFILE%\.codex\skills\codex-hosts`.

### How to use

1. After the skill and executable are installed correctly, Codex opens codex-hosts and asks you to enter a password when it needs a new remote connection.
2. You can also enter the connection details in codex-hosts beforehand, then ask Codex to identify and use the saved information to connect.

## Codex integration

The repository includes a portable Codex skill in `skill\codex-hosts`. After installation, its `SKILL.md` resolves `bin\codex-hosts.exe` relative to the installed skill directory, so no repository-specific path edit is required.

GUI edit mode accepts non-secret prefill values:

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

Never pass a password or private-key passphrase through chat, command-line arguments, JSON, scripts, or logs. The user enters credentials only in the masked GUI fields.

Tool mode uses the same executable to read a no-credential request file and write a result file:

```json
{"action":"list_hosts"}
{"action":"probe","alias":"example"}
{"action":"exec","alias":"example","command":"hostname"}
```

`probe` and `exec` may include per-operation `connect_timeout_ms` and `command_timeout_ms`. Omit them when no application-level timeout is needed. Saved hosts do not contain fixed timeouts, and remote commands are sent verbatim without guessing the remote operating system.

## Security boundaries

- Passwords and private-key passphrases are stored only in Windows Credential Manager and are never returned to Codex.
- SSH fingerprints are pinned explicitly and are never replaced automatically.
- Only verified SSH profiles can be jump hosts; chains are checked for cycles and limited to eight hosts.
- Telnet is plaintext and should be used only on a trusted network where that risk is explicitly accepted.
- Captured command output is limited to one MiB per output stream.
