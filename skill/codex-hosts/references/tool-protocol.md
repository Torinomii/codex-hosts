# Tool protocol

Read this when you need exact parameters, result fields, limits, or an error code that `SKILL.md` does not explain. The server's `initialize` instructions carry the live limits; prefer them when versions differ.

## Tools

| tool | required | optional |
|---|---|---|
| `list_hosts` | | `tags` (array; a host must carry every tag, compared ignoring case) |
| `agent_identities` | | |
| `fido_identities` | | |
| `probe` | `alias` | `connect_timeout_ms`, `command_timeout_ms` |
| `exec` | `alias`, `command` | `connect_timeout_ms`, `command_timeout_ms` |
| `exec_stdin` | `alias`, `command`, `stdin` | `connect_timeout_ms`, `command_timeout_ms` |
| `exec_many` | `alias`, `commands` (1–64, none empty) | `max_concurrency`, `connect_timeout_ms`, `command_timeout_ms` |
| `batch_probe` | `aliases` (1–256, unique) | `max_concurrency`, `connect_timeout_ms`, `command_timeout_ms`, `batch_timeout_ms`, `continue_on_error` (default true) |
| `batch_exec` | `aliases`, `command` | same as `batch_probe` |
| `disconnect` | | `alias` (drop only sessions involving this host; omit for all) |
| `open_host_editor` | `alias` | `host`, `port`, `user`, `protocol`, `auth`, `key_path`, `agent_key_fingerprint`, `jump_host`, `description`, `tags`, `observed_fingerprint`, `observed_algorithm` |
| `temporary_secrets_open` | `fields` | see [temporary-secrets.md](temporary-secrets.md) |
| `temporary_secrets_status` | `session` | |
| `temporary_secrets_run` | `session`, `program`, `cwd`, `env` | `args`, `timeout_ms` |
| `temporary_secrets_clear` | `session` | `fields` (empty clears all) |

- Timeouts are milliseconds and capped at 24 hours. Omitted values use the host profile's defaults, else 120 000 ms to connect and 600 000 ms per command; batches have no default deadline. `connect_timeout_ms` bounds the TCP connection, handshake, and authentication of each hop separately; `command_timeout_ms` bounds the remote command; `batch_timeout_ms` bounds the whole batch and hosts that have not started when it expires fail with `BATCH_TIMEOUT`.
- `max_concurrency` defaults to 8 and is clamped to 1–16 and to the host's `max_channels`.
- `stdin` (only `exec_stdin`, SSH hosts) is a UTF-8 string of at most 1 MiB, sent byte-for-byte with no added newline and followed by EOF.
- Aliases are 1–256 bytes and matched ignoring ASCII case and surrounding whitespace.
- Tool annotations: `list_hosts`, `agent_identities`, `fido_identities`, `probe`, `batch_probe`, and `temporary_secrets_status` are read-only (`probe` still updates local trust metadata); every `exec*`, `batch_exec`, and `temporary_secrets_run` executes code; `disconnect` and `temporary_secrets_clear` are idempotent.

## Results

Every result is returned as `structuredContent` and as the same JSON in the text content. Every result carries `schema_version` (currently 1) and `status`; failures set `isError`.

`list_hosts`: `hosts`, each with `alias`, `description`, `tags`, `address`, `port`, `username`, `protocol` (`ssh` or `telnet`), `ssh_auth` (SSH only), `agent_key_fingerprint` (only when set on an `ssh_agent` host), `jump_host` (alias, when set), `verified`, `auth_persistence` (`{"mode":"per_call"}`, `{"mode":"session"}`, or `{"mode":"idle","minutes":N}`), `max_channels`, `remote_env` (`auto`, `posix`, `windows`), `has_required_secret`, `has_host_fingerprint`, `host_key_algorithm`. Hosts hidden from Codex are omitted.

- `has_required_secret` is true for `private_key` and `ssh_agent` hosts; for password and Telnet hosts it reflects Windows Credential Manager for the current user.
- `verified` is true only after a successful probe, batch probe, or GUI test with the pinned key. A host accepted through the host-key prompt is pinned but not yet verified.

`probe`, `exec`, `exec_stdin`: `alias`, `exit_code`, `stdout`, `stderr`, `output_truncated`, and for SSH `host_fingerprint`, `host_key_algorithm`, `auth_key_fingerprint` (the public key that authenticated, for key and Agent routes), and `verified_host_keys` for each hop. `status` is `ok` when `exit_code` is 0 and `remote_error` otherwise. Telnet always reports `exit_code` 0.

`exec_many`: `alias`, `results` in input order with `index`, `status` (`ok`, `remote_error`, `error`, or `cancelled` with `COMMAND_CANCELLED`), `exit_code`, `stdout`, `stderr`, `output_truncated`, `error_code`, `error_message`; overall `status` is `ok`, `completed_with_remote_errors`, or `completed_with_errors`.

`batch_probe` and `batch_exec`: `action`, `duration_ms`, `results` in input order (each a normal result or a failure with `host_alias`), and `summary` with `requested`, `succeeded`, `remote_errors`, `failed`, `cancelled`. Overall `status` is `ok`, `completed_with_remote_errors`, or `completed_with_errors`. Hosts that never started after `continue_on_error:false` report `BATCH_CANCELLED`.

`agent_identities`: `identities` with `fingerprint`, `algorithm`, `comment`, `certificate`, `public_key`. `fido_identities`: `helper_available` (whether the Windows OpenSSH FIDO component exists) and `identities` with `path`, `fingerprint`, `algorithm`, `public_key`.

`disconnect`: `disconnected` (sessions closed). `open_host_editor`: `status` (`saved`, `trusted`, `cancelled`) and `alias`.

Failures are `{"status":"error","code":"...","message":"..."}` plus `host_alias` when a specific host or hop is known, and `expected_fingerprint`, `observed_fingerprint`, `expected_algorithm`, `observed_algorithm` for host-key failures.

## Limits

- A single command captures at most 1 MiB of combined stdout and stderr. `exec_many` shares one 8 MiB capture budget across its commands, and each batch host captures at most 8 MiB divided by the host count, never more than 2 MiB. Results are then trimmed so the complete JSON stays within 8 MiB, marking trimmed entries `output_truncated`.
- The SSH pool keeps up to 16 authenticated connections and may temporarily use 32 while all are busy. After each call the server closes every connection the call used unless the host's `auth_persistence` keeps it; kept sessions send keepalives and are closed by `disconnect`, by their idle limit, or when the server exits with Codex.
- At most 4 calls run at once. Calls to the same per-call host run one after another (each authenticates again); calls to a host that keeps its session share it, up to its `max_channels`; a batch runs alone.
- Hardware-key and Agent signing is serialized across processes: only one authentication prompt runs at a time, and authenticated channels then run concurrently.
- A cancelled call returns `CANCELLED` and closes its channel; a remote command that already started may keep running. A batch that is cancelled stops starting hosts.

## Error codes

Request validation: `REQUEST_INVALID`, `ALIAS_INVALID`, `ALIAS_NOT_FOUND`, `HOST_HIDDEN` (hidden from Codex in its profile), `PROFILE_INVALID` (the saved host is incomplete; open the editor), `COMMANDS_REQUIRED`, `TOO_MANY_COMMANDS`, `COMMAND_INVALID`, `BATCH_ALIASES_REQUIRED`, `BATCH_TOO_LARGE`, `BATCH_ALIAS_INVALID`, `STDIN_TOO_LARGE`, `STDIN_UNSUPPORTED` (stdin on a Telnet host), `SSH_REQUIRED` (`exec_many` on a Telnet host), `STORE_READ_FAILED`, `STORE_WRITE_FAILED`.

Connection and host keys: `CONNECT_FAILED`, `CONNECT_TIMEOUT`, `HANDSHAKE_FAILED`, `HOSTKEY_MISSING`, `HOSTKEY_UNKNOWN`, `HOSTKEY_MISMATCH`, `INVALID_HOST_CHAIN` (a hop is missing, not SSH, cyclic, or deeper than eight), `JUMP_CHANNEL_FAILED`, `SSH_CONNECTION_POOL_FULL`, `SSH_POOL_FAILED`, `SSH_BATCH_RECONNECT_BLOCKED`.

Authentication: `AUTH_TIMEOUT`, `CREDENTIAL_MISSING`, `CREDENTIAL_READ_FAILED`, `AUTH_FAILED`, `PRIVATE_KEY_LOAD_FAILED`, `FIDO_KEY_INVALID`, `FIDO_SIGNING_FAILED`, `FIDO_AUTH_REJECTED`, `HARDWARE_AUTH_LOCK_FAILED`, `SSH_AGENT_UNAVAILABLE`, `SSH_AGENT_NO_KEYS`, `SSH_AGENT_KEY_NOT_FOUND`, `SSH_AGENT_AUTH_FAILED`, `SSH_AGENT_SIGNING_REJECTED`, `SSH_AGENT_AUTH_REJECTED`, `SSH_AGENT_TIMEOUT` (identity discovery exceeded five seconds).

Execution: `CHANNEL_OPEN_FAILED`, `REMOTE_EXEC_FAILED` (the server rejected the exec request), `STDIN_WRITE_FAILED` (the channel closed before all input was sent and no exit status arrived; the command may have partly run), `COMMAND_TIMEOUT`, `OPERATION_TIMEOUT`, `CANCELLED`, `COMMAND_CANCELLED`, `COMMAND_WORKER_FAILED`, `BATCH_TIMEOUT`, `BATCH_CANCELLED`, `BATCH_INTERNAL_FAILED`, `BATCH_RESULT_TOO_LARGE`, `EXEC_MANY_RESULT_TOO_LARGE`, `SERIALIZE_FAILED`.

Editor and secrets: `EDITOR_ALIAS_REQUIRED`, `EDITOR_LAUNCH_FAILED`, `EDITOR_TIMEOUT` (open for an hour), `EDITOR_NO_RESULT` (closed without a result; not saved), `EDITOR_RESULT_INVALID`, `TEMPORARY_WINDOW_FAILED`, `TEMPORARY_WINDOW_TIMEOUT`, `TEMPORARY_SESSION_UNAVAILABLE`.

Telnet: `TELNET_CLOSED`, `TELNET_READ_FAILED`, `TELNET_WRITE_FAILED`, `OUTPUT_LIMIT`, `RUNTIME_CREATE_FAILED`.
