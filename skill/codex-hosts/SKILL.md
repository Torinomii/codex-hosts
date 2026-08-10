---
name: codex-hosts
description: Use when Codex needs to create, edit, trust, list, probe, or execute through saved SSH or Telnet hosts on Windows, including password SSH, encrypted OpenSSH key files, direct FIDO handles, SSH Agent/Pageant identities, pinned host keys, jump chains, and bounded multi-host batch work. Routes GUI and runtime work through one codex-hosts executable while keeping passwords, passphrases, PINs, and private-key material out of chat, arguments, request files, results, and logs.
---

# Codex Hosts

Resolve `bin\codex-hosts.exe` relative to this `SKILL.md`. Use that executable for GUI and tool work; do not substitute OpenSSH, Plink, Paramiko, a tunnel, daemon, or separate runtime.

## Protect credentials

- Never place passwords, key-file passphrases, or hardware PINs in chat, arguments, request/result JSON, scripts, logs, or project files.
- Let the user enter secrets only in masked GUI fields. Passwords and key-file passphrases may be stored in Windows Credential Manager; FIDO PINs are used only for the current operation and are never stored.
- Treat aliases, addresses, ports, user names, key paths, public keys, SHA-256 fingerprints, algorithms, and jump aliases as non-secret.
- Never recover credentials from history, backups, memory, or remote logs.
- Never add or enable startup, autorun, scheduled tasks, services, or Agent forwarding.

## Discover capabilities and hosts

1. Run `capabilities` once when the executable version or protocol is unknown.
2. Run `list_hosts` once per task and reuse that immutable snapshot for planning.
3. Reuse the exact requested alias when complete. Never interpret an omitted or empty alias list as all hosts.
4. If an entry is missing or incomplete, launch visible `--codex-edit` with its exact alias and known non-secret fields. Wait for its process and result; continue only after `saved`, and stop after `cancelled`.

Use a unique temporary result path. Delete only the exact request/result files created for the operation.

```text
--codex-edit --alias <alias> --host <host> --port <port> --user <user> --protocol <ssh|telnet> --auth <password|private-key|ssh-agent> --key-path <path> --agent-key-fingerprint <SHA256:...> --jump-host <alias> --result-file <result.json>
```

Omit unknown fields. Never add a password or passphrase argument.

The stable protocol names do not match the full GUI labels: `private-key` means **Key file / hardware key**, while `ssh-agent` means **SSH Agent / Pageant**. Do not describe `ssh-agent` as the general hardware-key option.

## Invoke tool mode

Write a no-secret UTF-8 JSON request, invoke the GUI-subsystem executable as
`--tool-request <request.json> --tool-result <result.json>` with hidden
`Start-Process -Wait -PassThru`, then read the structured result. Keep GUI edit
and host-key confirmation launches visible. `--result-file` is only for GUI edit
and host-key confirmation mode; tool mode rejects it.

```json
{"action":"capabilities"}
{"action":"agent_identities"}
{"action":"fido_identities"}
{"action":"list_hosts"}
{"action":"probe","alias":"example","connect_timeout_ms":5000}
{"action":"exec","alias":"example","command":"hostname","command_timeout_ms":10000}
{"action":"exec_many","alias":"example","commands":["hostname","uptime"],"max_concurrency":8,"command_timeout_ms":10000}
{"action":"batch_probe","aliases":["web-1","web-2"],"max_concurrency":8,"batch_timeout_ms":30000}
{"action":"batch_exec","aliases":["web-1","web-2"],"command":"uptime","max_concurrency":8,"continue_on_error":true,"batch_timeout_ms":30000}
```

- `exec` performs the pinned host-key check and authentication itself. For a saved, trusted host, do not add a separate `probe` before every ordinary command.
- Use `exec_many` for 2–64 independent short commands on one SSH host. It authenticates once and opens concurrent channels on the same connection; preserve result order by `index`.
- `batch_probe` and `batch_exec` require an explicit non-empty list of at most 256 aliases, preserve input order, default to concurrency 8, and cap concurrency at 16.
- Use one batch action instead of launching concurrent single-host processes. Use `continue_on_error:false` only when later hosts must not start after a failure.
- Set connect, command, and whole-batch timeouts when the task has meaningful bounds. Do not retry commands automatically because they may be non-idempotent.
- `exec_many` and batch output each have an eight-MiB budget for the complete serialized JSON result. Check each result's `output_truncated` field.
- Unknown or changed host keys remain per-host failures and are never batch-trusted.
- The running process retains up to 16 authenticated SSH connections, may temporarily use 32 while all are busy, closes idle connections after five minutes, and evicts least-recently-used idle connections first. This is in-process reuse, not a service; exiting the process closes it.
- Hardware-key and Agent authentication are single-file: allow only one authentication prompt at once, then let authenticated channels run concurrently. A batch shares the same authenticated jump-host session across targets.
- On `SSH_BATCH_RECONNECT_BLOCKED`, report the affected hosts. Do not automatically restart the batch or reconnect the shared jump host.

## Handle host keys

- Pin the SHA-256 fingerprint and record the host-key algorithm. Never edit stored JSON or replace a pin automatically.
- On `HOSTKEY_UNKNOWN` or `HOSTKEY_MISMATCH`, launch the exact alias visibly with both `--observed-fingerprint <value>` and `--observed-algorithm <value>`. Continue only after `trusted`, then retry once.
- Successful probes update first-seen and last-verified timestamps only when the freshly reloaded host still has the same connection details and fingerprint.
- A jump chain verifies every hop. Report the named hop on `INVALID_HOST_CHAIN` or `JUMP_CHANNEL_FAILED`.

## Use hardware-backed keys

- Treat the three routes as distinct: an ordinary OpenSSH key file is read by codex-hosts; a standard `*_sk` FIDO handle is signed directly with the connected hardware; an `ssh_agent` profile delegates to a separately running Windows OpenSSH Agent or Pageant.
- Prefer `fido_identities` for standard OpenSSH FIDO handle files. It scans saved handle files and does not prove the matching hardware is connected. Save the profile as `private_key` with the returned handle path; codex-hosts signs through the Windows system helper without an Agent, Pageant, vendor software, or background service.
- Use `agent_identities` only for identities already exposed by a running Windows OpenSSH Agent or Pageant. Select the intended identity by SHA-256 fingerprint and save the profile as `ssh_agent`.
- For new credentials created by this Windows app, use its ECDSA-SK path because it is compatible with the built-in Windows OpenSSH WebAuthn provider. Existing standard ECDSA-SK and Ed25519-SK files remain supported. Do not silently fall back to a different Agent key.
- Existing Windows/Google passkeys are not SSH identities. Use the visible GUI **Create / recover FIDO SSH** flow when no SSH-specific handle exists; secrets must remain in masked GUI fields.
- A resident/discoverable credential allows the local handle to be recovered on another supported system; it does not make the hardware private key portable. A non-resident credential cannot be recovered without its original handle file.
- Copy the generated public key and authorize it on every intended SSH server before testing. Creating a credential alone does not grant server access.
- Expect a touch or PIN prompt during direct FIDO signing. Group short commands with `exec_many` so one authenticated session can serve them without repeated prompts. The hardware private key never leaves the device, and Agent forwarding remains disabled.
- If `SSH_AGENT_UNAVAILABLE`, `SSH_AGENT_NO_KEYS`, or `SSH_AGENT_KEY_NOT_FOUND` occurs, report it and let the user configure or load their existing Agent. Do not start, enable, or persist an Agent service.

## Respect the server environment

- Do not infer the remote operating system or shell from SSH alone. Use user-provided or already verified environment facts before choosing commands.
- Partition heterogeneous hosts by compatible shell and command semantics before `batch_exec`; never send one POSIX command to a known Windows group or vice versa.
- Do not assume Bash, GNU utilities, `sudo`, systemd, UTF-8 locale, or an interactive TTY. Prefer minimal non-interactive commands supported by the known target.
- Use `probe` when the user explicitly asks to test a host, when a host key still needs confirmation, or while diagnosing a route failure. A normal `exec` on an already trusted host does not need a preliminary probe. Do not repeatedly retry a stable structured failure.

## Interpret failures

- `CREDENTIAL_MISSING`: open the exact alias for masked input.
- `AUTH_FAILED` or `PRIVATE_KEY_LOAD_FAILED`: open the exact alias to correct the selected OpenSSH key/FIDO-handle path or file passphrase. Never put a hardware PIN in the file-passphrase field.
- `FIDO_*`: distinguish an unreadable handle, missing Windows OpenSSH FIDO component, device/PIN/touch failure, and server public-key rejection. Do not suggest installing YubiKey Manager or enabling an Agent for the direct-handle route.
- `SSH_AGENT_*`: preserve the selected fingerprint and report the separately running Agent/Pageant state. Do not imply this error applies to direct FIDO handles.
- `CONNECT_TIMEOUT`, `COMMAND_TIMEOUT`, `OPERATION_TIMEOUT`, or `BATCH_TIMEOUT`: report the phase and preserve the host.
- `CreateProcessAsUserW failed: 1920` before launch is a local runner failure. Make one simpler retry, then one necessary approved retry; do not rewrite the host or route.

Completion requires a GUI result or structured tool result and confirmation that no credential entered any artifact.
