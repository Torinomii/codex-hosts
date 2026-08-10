# codex-hosts

<p align="center">
  <a href="https://github.com/Torinomii/codex-hosts/actions/workflows/release.yml"><img src="https://github.com/Torinomii/codex-hosts/actions/workflows/release.yml/badge.svg" alt="Build release" /></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/License-Apache--2.0-D22128.svg" alt="Apache 2.0 License" /></a>
  <img src="https://img.shields.io/badge/Windows-10%2B-0078D4?logo=windows&logoColor=white" alt="Windows 10 or newer" />
  <img src="https://img.shields.io/badge/Rust-1.92%2B-000000?logo=rust&logoColor=white" alt="Rust 1.92 or newer" />
  <a href="https://linux.do/"><img src="https://img.shields.io/badge/LINUX-DO-FFB003" alt="LINUX DO Community" /></a>
</p>

[English](README.md) | [简体中文](docs/readme/README_zh-CN.md) | [繁體中文](docs/readme/README_zh-TW.md) | [日本語](docs/readme/README_ja.md)

`codex-hosts` is a Windows SSH / Telnet host manager for Codex. It lets Codex connect safely without handling your passwords or keys.

![Codex Hosts main window](Main.png)

## What it does

- Saves server details for Codex to use when connecting.
- Supports passwords, ordinary OpenSSH keys, and hardware-backed keys such as FIDO/YubiKey.
- Supports identities already loaded in Windows OpenSSH Agent or Pageant.

## Installation

### Download a release

The prebuilt version supports 64-bit Windows 10 or newer. Download it from [Releases](https://github.com/Torinomii/codex-hosts/releases/latest).

To install it manually:

1. Put `bin\codex-hosts.exe` at `skill\codex-hosts\bin\codex-hosts.exe`.
2. Copy the complete `skill\codex-hosts` folder to `%USERPROFILE%\.codex\skills\codex-hosts`.

You can also ask Codex to install it:

```text
Download and install the latest codex-hosts release from https://github.com/Torinomii/codex-hosts/releases/latest. Automatically find the Skill installation directory for the current environment, install the complete Skill and executable, and confirm that all required files are in place.
```

### Build from source

Source builds require Rust 1.92 or newer and the MSVC toolchain:

```powershell
git clone https://github.com/Torinomii/codex-hosts.git
cd codex-hosts
cargo build --locked --release
```

When the build finishes, copy `target\release\codex-hosts.exe` to `skill\codex-hosts\bin\codex-hosts.exe`, then install the complete Skill folder.

## Quick start

1. Open `codex-hosts.exe` and create a host.
2. Enter an alias, address, port, and user name, then choose how to sign in and save.

<details>
<summary>Interface</summary>

### Calling from Codex or a script

GUI edit mode can be prefilled with non-secret connection details:

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

Authentication arguments use stable names:

- `password`: password authentication.
- `private-key` / `private_key`: a key file or FIDO handle; `fido-handle` is also accepted.
- `ssh-agent` / `ssh_agent`: a running SSH Agent or Pageant.

Tool mode reads a request file and writes a result file. Neither file may contain credentials:

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

`exec_many` authenticates once, then runs several short commands through concurrent channels on the same SSH connection. With a hardware key, this normally reduces a group of commands to one authentication. `agent_identities` and `fido_identities` return only public identity details and public keys. When running remote commands, check `output_truncated` in the result to see whether any output was shortened by the size limit.

</details>

## Security boundaries

- Passwords and key-file passphrases are stored only in Windows Credential Manager. A FIDO PIN is used only for the current operation and is never saved.
- SSH host fingerprints must be explicitly confirmed by the user. The app never replaces a saved fingerprint automatically.
- Direct FIDO signing never starts or enables an Agent service, and Agent forwarding is always disabled.
- Only verified SSH hosts can be used as jump hosts. A chain can contain at most eight hosts and is checked for loops.
- Telnet sends accounts and data in plaintext. Use it only on a trusted network where you explicitly accept that risk.
- A single command can capture at most 1 MiB of output. A complete `exec_many` or batch JSON result is limited to 8 MiB. The budget is applied while output is received, and the result is written directly instead of building several large JSON copies in memory.
