# Legacy tool file protocol

`codex-hosts.exe` keeps the request-file / result-file interface from 0.3.0 unchanged so a
Skill that has not been updated keeps working against a newer executable. New integrations
use the MCP server (`codex-hosts.exe --mcp`); see the Skill in `skill/codex-hosts`. Nothing in
this document is required for MCP use.

## Tool mode

Write a UTF-8 JSON request without secrets, run the executable with `--tool-request <request.json> --tool-result <result.json>`, then read the result file. It is a GUI-subsystem binary that prints nothing, so run it hidden with `Start-Process -Wait -PassThru`. Use unique temporary paths and delete only the files you created. Exit code 0 means success, 1 a structured failure in the result file, and 2 that no result could be written. `--result-file` belongs to the editor; tool mode ignores it.

```json
{"action":"capabilities"}
{"action":"list_hosts"}
{"action":"list_hosts","tags":["prod","web"]}
{"action":"probe","alias":"example","connect_timeout_ms":5000}
{"action":"exec","alias":"example","command":"hostname","command_timeout_ms":10000}
{"action":"exec","alias":"example","command":"python3 -","stdin":"print('hello')\n","command_timeout_ms":10000}
{"action":"exec_many","alias":"example","commands":["hostname","uptime"],"max_concurrency":8,"command_timeout_ms":10000}
{"action":"batch_probe","aliases":["web-1","web-2"],"max_concurrency":8,"batch_timeout_ms":30000}
{"action":"batch_exec","aliases":["web-1","web-2"],"command":"uptime","max_concurrency":8,"continue_on_error":true,"batch_timeout_ms":30000}
```

- `exec` performs the pinned host-key check and authentication itself, so a trusted host needs no preliminary `probe`. Use `probe` (it runs `hostname`) when the user asks to test a host, when a host key still needs confirmation, or while diagnosing a route. Do not repeat a stable structured failure.
- A result `status` is `ok`, `remote_error` (non-zero `exit_code`), or `error` with a `code`. Check `output_truncated`: a single command captures at most 1 MiB, and a complete `exec_many` or batch result is capped at 8 MiB.
- Optional `stdin` on single-host SSH `exec` sends the exact UTF-8 text (up to 1 MiB, no added newline) followed by EOF; `""` sends EOF at once. A remote program may exit before reading all of it, and its exit status stands. Telnet, `exec_many`, and `batch_exec` reject `stdin`.
- Set `connect_timeout_ms`, `command_timeout_ms`, and `batch_timeout_ms` when the task has real bounds. `connect_timeout_ms` also bounds authentication, so leave room for a touch or PIN prompt on hardware-backed hosts.
- Never retry a command automatically. It may not be idempotent, and a timeout or `STDIN_WRITE_FAILED` can mean it partly ran. Report the failure and let the user decide.
- Every tool process starts with no connections and authenticates again, which repeats hardware prompts; `exec_many` authenticates once and runs its commands on one connection. Batches keep input order, never trust host keys, and share one authenticated jump-host session. On `SSH_BATCH_RECONNECT_BLOCKED` report the affected hosts instead of restarting. Use `continue_on_error:false` only when later hosts must not start after a failure.

Read [references/tool-protocol.md](references/tool-protocol.md) for the complete request and result fields, limits, and error codes.

## Host editor launch

```text
--codex-edit --alias <alias> --host <host> --port <port> --user <user> --protocol <ssh|telnet> --auth <password|private-key|ssh-agent> --key-path <path> --agent-key-fingerprint <SHA256:...> --jump-host <alias> --description <notes> --tag <tag> --tag <tag> --result-file <result.json>
```

- Omit unknown fields. Never add a password, passphrase, or PIN argument; the editor has no such flags and the process rejects unknown arguments with exit code 2.
- Use the exact alias the user gave. When the alias exists, its saved profile opens and the given fields overwrite the matching draft fields; when it does not exist, a new profile is created and saved immediately with the given fields. Without `--alias` the app picks a neutral `host-N` name.
- `--auth` accepts `password`, `private-key` (also `fido-handle`), and `ssh-agent`. The stable names differ from the GUI labels: `private-key` is **Key file / hardware key** and `ssh-agent` is **SSH Agent / Pageant**. Do not describe `ssh-agent` as the general hardware-key option.
- `--jump-host` should name an existing verified SSH alias, the only kind the editor offers. An unknown or empty value clears the draft's jump host, so omit the flag when the chain should stay as saved.
- Metadata prefill: omitting `--description` or `--tag` keeps the saved values, `--description ""` clears the notes, and a single `--tag ""` clears the tags. Tags are trimmed and de-duplicated ignoring case, keeping the first spelling.

For host-key confirmation, launch the same alias with the values from the `HOSTKEY_UNKNOWN` or `HOSTKEY_MISMATCH` failure:

```text
--codex-edit --alias <alias> --observed-fingerprint <observed_fingerprint> --observed-algorithm <observed_algorithm> --result-file <result.json>
```

The alias must already exist; otherwise the window shows `HOST_NOT_FOUND`, no prompt appears, and closing it yields `cancelled`. Accepting stores the new fingerprint and algorithm but leaves the host unverified until a later successful probe or test.

## Run the window as the interactive user

- Launch with `Start-Process -WindowStyle Normal -PassThru` as the interactive Windows user who owns the profiles and credentials. A sandbox process may start without a window on that user's desktop. When that happens, use the platform's approved interactive-user execution path; keep hidden launches for tool mode only.
- Verify that the window is visible with the available desktop or window inspection before saying it is open. A PID, a successful `Start-Process`, or a nonzero window handle does not prove visibility. If visibility cannot be inspected, report only that the launch was requested and keep the result pending.
- Keep the process handle and the unique result path, and wait in bounded intervals while the user enters values. A tool call that yields a session ID is still running.
- Before relaunching an apparently invisible window, inspect the original process and result file to avoid duplicate editors or losing a completed save. Do not terminate unrelated app instances or temporary-secret sessions.

## Read the result

The result file is written once, when the window closes:

```json
{"status":"saved","alias":"example"}
```

- `saved`: the profile and any entered credentials are stored; continue with the host work.
- `trusted`: the observed host key is now pinned; retry the failed operation once.
- `cancelled`: the user declined, or the window closed without saving; end that flow and report it.
- No file: the window has not closed, or the launch failed. A missing result is never success.

Delete only the result file you created.

## Request and result reference

Read this when you need exact request fields, result fields, limits, or an error code that `SKILL.md` does not explain. All values below match `capabilities` of the bundled executable; prefer the live `capabilities` result when versions differ.

### Requests

Every request is a JSON object with `action`. Unknown fields are ignored except in temporary-secret requests, which reject them.

| action | required | optional |
|---|---|---|
| `capabilities` | | |
| `list_hosts` | | `tags` (array; a host must carry every tag, compared ignoring case) |
| `agent_identities` | | |
| `fido_identities` | | |
| `probe` | `alias` | `connect_timeout_ms`, `command_timeout_ms` |
| `exec` | `alias`, `command` | `stdin`, `connect_timeout_ms`, `command_timeout_ms` |
| `exec_many` | `alias`, `commands` (1–64, none empty) | `max_concurrency`, `connect_timeout_ms`, `command_timeout_ms` |
| `batch_probe` | `aliases` (1–256, unique) | `max_concurrency`, `connect_timeout_ms`, `command_timeout_ms`, `batch_timeout_ms`, `continue_on_error` (default true) |
| `batch_exec` | `aliases`, `command` | same as `batch_probe` |
| `temporary_secrets_open` | `fields` | see [temporary-secrets.md](temporary-secrets.md) |
| `temporary_secrets` | `session`, `request` | see [temporary-secrets.md](temporary-secrets.md) |

- Timeouts are milliseconds and capped at 24 hours. `connect_timeout_ms` bounds the TCP connection, handshake, and authentication of each hop separately; `command_timeout_ms` bounds the remote command; `batch_timeout_ms` bounds the whole batch and hosts that have not started when it expires fail with `BATCH_TIMEOUT`.
- `max_concurrency` defaults to 8 and is clamped to 1–16.
- `stdin` is accepted only by `exec` on an SSH host: a UTF-8 string of at most 1 MiB, sent byte-for-byte with no added newline and followed by EOF. `null` or omission sends nothing, which is the legacy behaviour.
- Aliases are 1–256 bytes and matched ignoring ASCII case and surrounding whitespace.

### Results

Every result carries `schema_version` (currently 1) and `status`.

`capabilities`: `app_version`, `actions`, `ssh_auth` (`password`, `private_key`, `ssh_agent`), `max_batch_hosts`, `max_batch_concurrency`, `default_batch_concurrency`, `max_batch_output_bytes`, `max_exec_many_commands`, `max_timeout_ms`, `default_retained_connections`, `max_retained_connections`, `connection_idle_timeout_ms`, `host_key_hash` (`sha256`), `agent_forwarding` (false), `host_metadata_fields`, `list_hosts_tag_filter`, `exec_stdin`, `exec_stdin_protocols`, `max_stdin_bytes`.

`list_hosts`: `hosts`, each with `alias`, `description`, `tags`, `address`, `port`, `username`, `protocol` (`ssh` or `telnet`), `ssh_auth` (SSH only), `agent_key_fingerprint` (only when set on an `ssh_agent` host), `jump_host` (alias, when set), `verified`, `has_required_secret`, `has_host_fingerprint`, `host_key_algorithm`.

- `has_required_secret` is true for `private_key` and `ssh_agent` hosts; for password and Telnet hosts it reflects Windows Credential Manager for the current user, so a sandbox user can see false for a host the desktop user saved.
- `verified` is true only after a successful probe, batch probe, or GUI test with the pinned key. A host accepted through the host-key prompt is pinned but not yet verified.

`probe` and `exec`: `alias`, `exit_code`, `stdout`, `stderr`, `output_truncated`, and for SSH `host_fingerprint`, `host_key_algorithm`, `auth_key_fingerprint` (the public key that authenticated, for key and Agent routes), and `verified_host_keys` for each hop. `status` is `ok` when `exit_code` is 0 and `remote_error` otherwise. Telnet always reports `exit_code` 0.

`exec_many`: `alias`, `results` in input order with `index`, `status` (`ok`, `remote_error`, `error`, or `cancelled` with `COMMAND_CANCELLED`), `exit_code`, `stdout`, `stderr`, `output_truncated`, `error_code`, `error_message`; overall `status` is `ok`, `completed_with_remote_errors`, or `completed_with_errors`.

`batch_probe` and `batch_exec`: `action`, `duration_ms`, `results` in input order (each a normal result or a failure with `host_alias`), and `summary` with `requested`, `succeeded`, `remote_errors`, `failed`, `cancelled`. Overall `status` is `ok`, `completed_with_remote_errors`, or `completed_with_errors`. Hosts that never started after `continue_on_error:false` report `BATCH_CANCELLED`.

`agent_identities`: `identities` with `fingerprint`, `algorithm`, `comment`, `certificate`, `public_key`. `fido_identities`: `helper_available` (whether the Windows OpenSSH FIDO component exists) and `identities` with `path`, `fingerprint`, `algorithm`, `public_key`.

Failures are `{"status":"error","code":"...","message":"..."}` plus `host_alias` when a specific host or hop is known, and `expected_fingerprint`, `observed_fingerprint`, `expected_algorithm`, `observed_algorithm` for host-key failures.

### Limits

- A single command captures at most 1 MiB of combined stdout and stderr. `exec_many` shares one 8 MiB capture budget across its commands, and each batch host captures at most 8 MiB divided by the host count, never more than 2 MiB. Results are then trimmed so the complete pretty-printed JSON stays within 8 MiB, marking trimmed entries `output_truncated`.
- Within one process the SSH pool keeps up to 16 authenticated connections, may temporarily use 32 while all are busy, evicts idle connections least-recently-used first, and closes idle connections after five minutes. Each tool invocation is a fresh process, so pooling only helps `exec_many` and batches.
- Hardware-key and Agent signing is serialized across processes: only one authentication prompt runs at a time, and authenticated channels then run concurrently.

### Error codes

Request validation: `REQUEST_READ_FAILED`, `REQUEST_INVALID`, `ALIAS_INVALID`, `ALIAS_NOT_FOUND`, `PROFILE_INVALID` (the saved host is incomplete; open the editor), `COMMANDS_REQUIRED`, `TOO_MANY_COMMANDS`, `COMMAND_INVALID`, `BATCH_ALIASES_REQUIRED`, `BATCH_TOO_LARGE`, `BATCH_ALIAS_INVALID`, `STDIN_TOO_LARGE`, `STDIN_UNSUPPORTED` (stdin on Telnet, `exec_many`, or `batch_exec`), `SSH_REQUIRED` (`exec_many` on a Telnet host), `STORE_READ_FAILED`, `STORE_WRITE_FAILED`.

Connection and host keys: `CONNECT_FAILED`, `CONNECT_TIMEOUT`, `HANDSHAKE_FAILED`, `HOSTKEY_MISSING`, `HOSTKEY_UNKNOWN`, `HOSTKEY_MISMATCH`, `INVALID_HOST_CHAIN` (a hop is missing, not SSH, cyclic, or deeper than eight), `JUMP_CHANNEL_FAILED`, `SSH_CONNECTION_POOL_FULL`, `SSH_POOL_FAILED`, `SSH_BATCH_RECONNECT_BLOCKED`.

Authentication: `AUTH_TIMEOUT`, `CREDENTIAL_MISSING`, `CREDENTIAL_READ_FAILED`, `AUTH_FAILED`, `PRIVATE_KEY_LOAD_FAILED`, `FIDO_KEY_INVALID`, `FIDO_SIGNING_FAILED`, `FIDO_AUTH_REJECTED`, `HARDWARE_AUTH_LOCK_FAILED`, `SSH_AGENT_UNAVAILABLE`, `SSH_AGENT_NO_KEYS`, `SSH_AGENT_KEY_NOT_FOUND`, `SSH_AGENT_AUTH_FAILED`, `SSH_AGENT_SIGNING_REJECTED`, `SSH_AGENT_AUTH_REJECTED`, `SSH_AGENT_TIMEOUT` (identity discovery exceeded five seconds).

Execution: `CHANNEL_OPEN_FAILED`, `REMOTE_EXEC_FAILED` (the server rejected the exec request), `STDIN_WRITE_FAILED` (the channel closed before all input was sent and no exit status arrived; the command may have partly run), `COMMAND_TIMEOUT`, `OPERATION_TIMEOUT`, `COMMAND_CANCELLED`, `COMMAND_WORKER_FAILED`, `BATCH_TIMEOUT`, `BATCH_CANCELLED`, `BATCH_INTERNAL_FAILED`, `BATCH_RESULT_TOO_LARGE`, `EXEC_MANY_RESULT_TOO_LARGE`, `SERIALIZE_FAILED`.

Telnet: `TELNET_CLOSED`, `TELNET_READ_FAILED`, `TELNET_WRITE_FAILED`, `OUTPUT_LIMIT`, `RUNTIME_CREATE_FAILED`.
