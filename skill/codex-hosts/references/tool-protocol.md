# Tool protocol

Read this when you need exact request fields, result fields, limits, or an error code that `SKILL.md` does not explain. All values below match `capabilities` of the bundled executable; prefer the live `capabilities` result when versions differ.

## Requests

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

## Results

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

## Limits

- A single command captures at most 1 MiB of combined stdout and stderr. `exec_many` shares one 8 MiB capture budget across its commands, and each batch host captures at most 8 MiB divided by the host count, never more than 2 MiB. Results are then trimmed so the complete pretty-printed JSON stays within 8 MiB, marking trimmed entries `output_truncated`.
- Within one process the SSH pool keeps up to 16 authenticated connections, may temporarily use 32 while all are busy, evicts idle connections least-recently-used first, and closes idle connections after five minutes. Each tool invocation is a fresh process, so pooling only helps `exec_many` and batches.
- Hardware-key and Agent signing is serialized across processes: only one authentication prompt runs at a time, and authenticated channels then run concurrently.

## Error codes

Request validation: `REQUEST_READ_FAILED`, `REQUEST_INVALID`, `ALIAS_INVALID`, `ALIAS_NOT_FOUND`, `PROFILE_INVALID` (the saved host is incomplete; open the editor), `COMMANDS_REQUIRED`, `TOO_MANY_COMMANDS`, `COMMAND_INVALID`, `BATCH_ALIASES_REQUIRED`, `BATCH_TOO_LARGE`, `BATCH_ALIAS_INVALID`, `STDIN_TOO_LARGE`, `STDIN_UNSUPPORTED` (stdin on Telnet, `exec_many`, or `batch_exec`), `SSH_REQUIRED` (`exec_many` on a Telnet host), `STORE_READ_FAILED`, `STORE_WRITE_FAILED`.

Connection and host keys: `CONNECT_FAILED`, `CONNECT_TIMEOUT`, `HANDSHAKE_FAILED`, `HOSTKEY_MISSING`, `HOSTKEY_UNKNOWN`, `HOSTKEY_MISMATCH`, `INVALID_HOST_CHAIN` (a hop is missing, not SSH, cyclic, or deeper than eight), `JUMP_CHANNEL_FAILED`, `SSH_CONNECTION_POOL_FULL`, `SSH_POOL_FAILED`, `SSH_BATCH_RECONNECT_BLOCKED`.

Authentication: `AUTH_TIMEOUT`, `CREDENTIAL_MISSING`, `CREDENTIAL_READ_FAILED`, `AUTH_FAILED`, `PRIVATE_KEY_LOAD_FAILED`, `FIDO_KEY_INVALID`, `FIDO_SIGNING_FAILED`, `FIDO_AUTH_REJECTED`, `HARDWARE_AUTH_LOCK_FAILED`, `SSH_AGENT_UNAVAILABLE`, `SSH_AGENT_NO_KEYS`, `SSH_AGENT_KEY_NOT_FOUND`, `SSH_AGENT_AUTH_FAILED`, `SSH_AGENT_SIGNING_REJECTED`, `SSH_AGENT_AUTH_REJECTED`, `SSH_AGENT_TIMEOUT` (identity discovery exceeded five seconds).

Execution: `CHANNEL_OPEN_FAILED`, `REMOTE_EXEC_FAILED` (the server rejected the exec request), `STDIN_WRITE_FAILED` (the channel closed before all input was sent and no exit status arrived; the command may have partly run), `COMMAND_TIMEOUT`, `OPERATION_TIMEOUT`, `COMMAND_CANCELLED`, `COMMAND_WORKER_FAILED`, `BATCH_TIMEOUT`, `BATCH_CANCELLED`, `BATCH_INTERNAL_FAILED`, `BATCH_RESULT_TOO_LARGE`, `EXEC_MANY_RESULT_TOO_LARGE`, `SERIALIZE_FAILED`.

Telnet: `TELNET_CLOSED`, `TELNET_READ_FAILED`, `TELNET_WRITE_FAILED`, `OUTPUT_LIMIT`, `RUNTIME_CREATE_FAILED`.
