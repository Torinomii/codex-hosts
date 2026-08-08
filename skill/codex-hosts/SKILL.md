---
name: codex-hosts
description: Use when Codex needs to create, edit, trust, list, probe, or execute through stable saved SSH or Telnet hosts on Windows, including password-only SSH, encrypted private keys, and multi-hop SSH host chains. Routes GUI and runtime work through the single codex-hosts executable, pre-creates alias-specific drafts, waits for automatic Save, Trust, or Cancel results, and keeps passwords and key passphrases out of chat, arguments, files, and logs.
---

# Codex Hosts

Use the single Windows GUI executable at `bin\codex-hosts.exe` under this installed skill's directory. Resolve the directory containing this `SKILL.md`, then use its adjacent `bin` directory; do not assume a repository-specific absolute path. Do not substitute Windows OpenSSH, Plink, Paramiko, a newly deployed tunnel, a daemon, or a separate runtime.

## Protect secrets

- Never put passwords or private-key passphrases in chat, command-line arguments, request/result JSON, scripts, logs, or project files.
- Let the user enter or replace a secret only in the masked GUI field. The application stores it in Windows Credential Manager.
- A private-key path, alias, address, port, user name, protocol, authentication method, jump-host alias, and SHA-256 host fingerprint are non-secret and may be prefilled when already known.
- Never recover credentials from task history, shell history, configuration backups, memory, or remote logs.
- Never add startup, login, autorun, a scheduled task, or a Windows service.

## Resolve an alias before connecting

1. Run `list_hosts` before asking for connection details.
2. Reuse the exact requested alias when its saved entry is complete.
3. If it is missing or needs credentials, launch `--codex-edit` immediately with the exact alias and every known non-secret field. The application creates a missing draft before showing the GUI.
4. Do not ask the user to retype known values in chat or to send a “done” message. Start GUI edit mode visibly, keep its process handle, wait for it to exit, and then read its result file.
5. Continue only after `saved`; stop on `cancelled`. A missing or invalid result is a local GUI/result failure.

Use a unique result path in the current task's temporary directory. After reading it, delete only the exact temporary files created for this operation.

Example arguments; omit unknown fields rather than inventing them:

```text
--codex-edit --alias <alias> --host <host> --port <port> --user <user> --protocol <ssh|telnet> --auth <password|private-key> --key-path <path> --jump-host <saved-alias> --result-file <result.json>
```

Never add a password or passphrase argument. Unknown arguments are rejected.

## Tool mode

Write a no-secret UTF-8 JSON request, then invoke the same executable with `--tool-request <request.json> --tool-result <result.json>`.

Because this is a Windows GUI-subsystem executable, do not rely on a shell's `$LASTEXITCODE` to wait for it. For tool mode, use a hidden `Start-Process -Wait -PassThru` (or an equivalent process API), then read the result file. GUI edit and fingerprint-confirmation launches must remain visible, but must still be waited on through their process handle.

```json
{"action":"list_hosts"}
{"action":"probe","alias":"example"}
{"action":"exec","alias":"example","command":"hostname"}
```

`probe` and `exec` also accept optional `connect_timeout_ms` and `command_timeout_ms`. Omit them for no application timeout. Set them only when the current Codex operation has a meaningful bound; saved hosts never contain fixed timeouts.

Read the structured result before deleting the request and result files:

- `CREDENTIAL_MISSING`: open the exact alias with `--codex-edit` and wait for Save or Cancel.
- `HOSTKEY_UNKNOWN` or `HOSTKEY_MISMATCH`: launch the exact alias with `--observed-fingerprint <value>` and a result file. The GUI shows the previous and detected values. Continue only after `trusted`, then retry once; stop on `cancelled`.
- `AUTH_FAILED` or `PRIVATE_KEY_LOAD_FAILED`: open the exact alias so the user can correct the masked secret or key path.
- `INVALID_HOST_CHAIN` or `JUMP_CHANNEL_FAILED`: report the named chain failure. Do not replace it with a random route.
- `CONNECT_TIMEOUT` or `COMMAND_TIMEOUT`: report the phase and preserve the host.

The executable verifies unchanged pinned host keys without asking again. Never edit a stored fingerprint in JSON or bypass the GUI confirmation.

## Protocol behavior

- SSH password mode tries password authentication and then keyboard-interactive prompts.
- SSH private-key mode loads the configured OpenSSH private key from source-controlled Rust code; an optional passphrase comes from Windows Credential Manager.
- Host chaining follows saved, verified SSH jump hosts recursively. Telnet cannot use or provide a jump host.
- Telnet is plaintext. Use it only when the user selected it and the target network is trusted.
- Remote commands are sent verbatim. Choose a command suitable for the remote shell; the host entry does not guess or store a remote operating system.

Probe before the first command in a task or after a connection-path failure. Do not retry a stable structured failure repeatedly.

## Local launch failures

`CreateProcessAsUserW failed: 1920` before the executable starts is a local Codex runner failure, not a remote connection failure. Make at most one simplified retry, then one necessary explicitly approved retry. Do not redeploy the remote route or rewrite the saved host.

## Completion

Opening the GUI or writing a request file is not completion. Confirm the GUI result, confirm the requested structured probe/command result, and verify that no secret appeared in any artifact.
