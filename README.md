# codex-hosts

[English](README.md) | [简体中文](docs/readme/README_zh-CN.md) | [繁體中文](docs/readme/README_zh-TW.md) | [日本語](docs/readme/README_ja.md)

`codex-hosts` is a Windows SSH / Telnet host manager for Codex, allowing Codex to initiate connections securely without accessing or entering plaintext passwords.

![Codex Hosts main window](Main.png)

## Highlights

- Save an alias, address, port, user name, protocol, authentication method, optional private-key path, trusted SSH host key, and optional jump host.
- Use SSH password and keyboard-interactive authentication, or an OpenSSH private key with an optional passphrase.
- Store passwords and private-key passphrases only in Windows Credential Manager.
- Build multi-hop SSH chains from saved, verified SSH hosts.
- Use Telnet in legacy environments where its risks have been explicitly accepted; Telnet credentials and traffic are not encrypted.
- Let Codex pre-create an alias-specific draft and wait for Save, Trust, or Cancel without asking for credentials in chat.

## Installation

### Download a GitHub Actions release

The prebuilt package supports 64-bit Windows 10 or newer. A pushed `v*` tag builds `codex-hosts-windows-x86_64.zip` on a GitHub-hosted Windows runner and publishes it on the [Releases page](https://github.com/Torinomii/codex-hosts/releases/latest). Manually started runs also provide the same ZIP on the [Actions page](https://github.com/Torinomii/codex-hosts/actions/workflows/release.yml).

The minimal ZIP contains only:

- `bin\codex-hosts.exe`
- `skill\codex-hosts\SKILL.md`
- `skill\codex-hosts\agents\openai.yaml`

Run the GUI directly from `bin\codex-hosts.exe`. To install the Codex skill, copy `skill\codex-hosts` to `%USERPROFILE%\.codex\skills\codex-hosts`, then copy the EXE to `%USERPROFILE%\.codex\skills\codex-hosts\bin\codex-hosts.exe`.

### Build from source

Source builds require Rust 1.92 or newer with the MSVC toolchain:

```
git clone https://github.com/Torinomii/codex-hosts.git
cd codex-hosts
cargo build --locked --release
```

Copy `target\release\codex-hosts.exe` to `skill\codex-hosts\bin\codex-hosts.exe`, then install the complete `skill\codex-hosts` directory as `%USERPROFILE%\.codex\skills\codex-hosts`.

### General Codex installation prompt

```text
Install codex-hosts on this Windows computer. Prefer the latest GitHub Actions-built `codex-hosts-windows-x86_64.zip` from https://github.com/Torinomii/codex-hosts/releases/latest. Extract it, copy `skill\codex-hosts` to `$env:USERPROFILE\.codex\skills\codex-hosts`, and copy `bin\codex-hosts.exe` to `$env:USERPROFILE\.codex\skills\codex-hosts\bin\codex-hosts.exe`. Confirm that the EXE, `SKILL.md`, and `agents\openai.yaml` exist. If no compatible Release is available, or I explicitly request a source installation, clone the repository, use Rust 1.92 or newer with the MSVC toolchain, run `cargo build --locked --release`, place the built EXE at `skill\codex-hosts\bin\codex-hosts.exe`, and install that complete skill directory. Never request, store, or echo passwords or private-key passphrases in chat, command arguments, scripts, files, or logs. Do not create startup entries, scheduled tasks, services, or background daemons. After installation, launch the visible GUI once for acceptance and report the installation source, final EXE path, and file SHA-256.
```

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
