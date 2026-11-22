# Tool protocol

Use the running server's tool schemas and `initialize` instructions for the installed version.

## Tools

| tool | required | optional |
|---|---|---|
| `list_hosts` | | `tags` (a host must carry every tag, ignoring case) |
| `agent_identities` | | |
| `fido_identities` | | |
| `probe` | `alias` | `connect_timeout_ms`, `command_timeout_ms` |
| `exec` | `alias`, `command` | `connect_timeout_ms`, `command_timeout_ms` |
| `exec_stdin` | `alias`, `command`, `stdin` | `connect_timeout_ms`, `command_timeout_ms` |
| `exec_sudo` | `alias`, `program` (absolute path, SSH only), `user` | `args` (argv array), `connect_timeout_ms`, `command_timeout_ms` |
| `exec_su` | `alias`, `program` (absolute path, SSH only), `user` | `args` (argv array), `connect_timeout_ms`, `command_timeout_ms` |
| `exec_many` | `alias`, `commands` (1–64, none empty) | `max_concurrency`, `connect_timeout_ms`, `command_timeout_ms` |
| `batch_probe` | `aliases` (1–256, unique) | `max_concurrency`, `connect_timeout_ms`, `command_timeout_ms`, `batch_timeout_ms`, `continue_on_error` (default true) |
| `batch_exec` | `aliases`, `command` | same as `batch_probe` |
| `disconnect` | | `alias` (only sessions involving this host; omitted: all) |
| `open_host_editor` | `alias` | `host`, `port`, `user`, `protocol`, `auth`, `key_path`, `agent_key_fingerprint`, `jump_host`, `description`, `tags`, `sudo_enabled`, `sudo_users`, `su_enabled`, `observed_fingerprint`, `observed_algorithm` |
| `temporary_secrets_open` | `fields` | see [temporary-secrets.md](temporary-secrets.md) |
| `temporary_secrets_status` | `session` | |
| `temporary_secrets_run` | `session`, `program`, `cwd`, `env` | `args`, `timeout_ms` |

- Remote timeouts are milliseconds, capped at 24 hours. Connection and command timeouts use the host profile's defaults when set, otherwise 120,000 and 600,000 ms respectively. SSH applies `connect_timeout_ms` to individual connection, handshake and authentication phases. Telnet uses it for the TCP connection and uses `command_timeout_ms` for login and command execution together. The optional `batch_timeout_ms` starts after batch admission and any required approval. Workers share that deadline. A host picked up after expiry gets `BATCH_TIMEOUT`.
- `max_concurrency` defaults to 8 and is clamped to 1–16. `exec_many` also caps concurrent command channels by the host's `max_channels`. `batch_probe` and `batch_exec` cap host workers by the number of requested hosts.
- `stdin` (`exec_stdin`, SSH only) is UTF-8 of at most 1 MiB, sent byte for byte without an added newline, then EOF; `""` sends EOF at once. A program may exit before reading all of it; its exit status stands.
- Aliases are 1–256 bytes, matched ignoring ASCII case and surrounding whitespace.
- Annotations: `list_hosts`, `agent_identities`, `fido_identities`, `probe`, `batch_probe`, and `temporary_secrets_status` are read-only (`probe` still updates local trust metadata); every `exec*`, `batch_exec`, and `temporary_secrets_run` executes code; `disconnect` is idempotent.
- SSH call approval is a per-host Advanced switch, default off. If enabled on any SSH target or jump host, one blue GUI review shows the entire call's targets, jumps, exact commands, and supplied stdin or execution user. Approval covers only that call, including its batch workers; retained authentication never extends it. Overview Escape rejects the displayed batch; detail Back/Escape makes no decision. Tools cannot change this policy. See [approvals.md](approvals.md) for frozen batches, expiry and the independent temporary-secret approval flow.

## Results

`exec_sudo` and `exec_su` return the single-host result plus `sudo_user` or `su_user`, the execution identity required on that call. After READY, a nonzero program exit is `remote_error`. A missing READY marker, cancellation or timeout leaves execution uncertain. For password matching, remote requirements and failures, see [sudo](sudo.md) or [su](su.md).

Every tool rejects parameters outside its schema as invalid arguments, before any connection; for example, `stdin` belongs only to `exec_stdin`. Fix the call; do not drop the field and resend. Each result is `structuredContent` plus the same JSON as text and carries `schema_version` (currently 1) and `status`. Structured tool failures set `isError:true`; a completed remote program with nonzero exit code returns `status:"remote_error"` without setting MCP `isError:true`. Always inspect `status` and `exit_code`, not just `isError` or an approval message. A result belongs to its own call: commands have separate SSH channels (Telnet: separate connections), so late output from an earlier call is not delivered as a later call's result.

`list_hosts`: `hosts`, each with `alias`, `description`, `tags`, `address`, `port`, `username`, `protocol` (`ssh`, `telnet`), `ssh_auth` (SSH only), `credential_source` (`credential_manager`, `bitwarden`, or `onepassword`, only for hosts that log in with a password), `agent_key_fingerprint` (only when set on an `ssh_agent` host), `jump_host` (alias, when set), `verified`, `auth_persistence` (`{"mode":"per_call"}`, `{"mode":"session"}`, or `{"mode":"idle","minutes":N}`), `require_call_approval` (default false; false for Telnet), `max_channels` (cap on concurrent command sessions; tunnels through a jump host are not counted), `remote_env` (`auto`, `posix`, `windows`), `has_required_secret`, `has_host_fingerprint`, `host_key_algorithm`. The `sudo_enabled` and `su_enabled` fields describe independent permissions for saved-password supply. `sudo_login_password_available` indicates an eligible login password for sudo. Optional `sudo_credentials` entries carry `username` and `has_password`. Passwordless execution can work with supply off or no records. Hosts hidden from Codex are omitted and answer `HOST_HIDDEN`.

- `has_required_secret` is true for private-key, SSH Agent and vault-backed profiles by authentication type or reference. For a local password host it is false if the credential is missing or if Credential Manager cannot be read. Follow the login error for the affected host. `CREDENTIAL_MISSING` calls for the editor, while `CREDENTIAL_READ_FAILED` calls for checking the MCP process's Windows user and credential-store access.
- For vault-backed entries, see [vault discovery, unlock and caching](vault-hosts.md).
- `verified` is true only after a successful probe, batch probe, or GUI test with the pinned key. A key accepted in the editor is pinned but not yet verified.

`probe`, `exec`, `exec_stdin`: `alias`, `exit_code`, `stdout`, `stderr`, `output_truncated`; for SSH also `host_fingerprint`, `host_key_algorithm`, `auth_key_fingerprint` (the public key that authenticated, for key and Agent routes), and `verified_host_keys` per hop. `status` is `ok` when `exit_code` is 0, else `remote_error`. Telnet's `exit_code` is the status the remote shell prints after the command, or 0 when it prints none.

`exec_many`: `alias`, `results` in input order, each with `index`, `status` (`ok`, `remote_error`, `error`, or `cancelled` with `COMMAND_CANCELLED`), `exit_code`, `stdout`, `stderr`, `output_truncated`, `error_code`, `error_message`; overall `status` is `ok`, `completed_with_remote_errors`, or `completed_with_errors`.

`batch_probe`, `batch_exec`: `action`, `duration_ms`, `results` in input order (each a normal result, or a failure with `host_alias`), and `summary` with `requested`, `succeeded`, `remote_errors`, `failed`, `cancelled`; overall `status` as for `exec_many`. With `continue_on_error:false`, the first host that ends in `error` or `remote_error` (non-zero exit) stops new hosts from starting; hosts already running finish, and hosts never started report `BATCH_CANCELLED`.

`agent_identities`: `identities` with `fingerprint`, `algorithm`, `comment`, `certificate`, `public_key`. `fido_identities`: `helper_available` (whether the Windows OpenSSH FIDO component exists) and `identities` with `path`, `fingerprint`, `algorithm`, `public_key`.

`disconnect` closes retained SSH sessions at the user's request and returns `disconnected` (sessions closed now plus those that close after active calls release them). Already-started remote work may continue. `open_host_editor` returns `status` (`saved`, `trusted`, `cancelled`) and `alias`. An unknown local alias may be saved as a prefilled profile when the window opens. A cancellation or missing callback leaves earlier writes intact.

Failures: `{"status":"error","code":"...","message":"..."}`, plus `host_alias` when a specific host or hop is known, and `expected_fingerprint`, `observed_fingerprint`, `expected_algorithm`, `observed_algorithm` for host-key failures.

## Limits

- One command captures at most 1 MiB of stdout and stderr combined. `exec_many` shares one 8 MiB capture budget; each batch host captures at most 8 MiB divided by the host count, never more than 2 MiB. Results are then trimmed so the whole JSON stays within 8 MiB, marking trimmed entries `output_truncated`.
- The SSH pool keeps up to 16 authenticated connections, 32 while all are busy. After each call the server closes every connection the call used unless the host's `auth_persistence` keeps it. When planning continuous work, mention retained authentication. Kept sessions send keepalives and close on `disconnect`, their idle limit, or server exit with Codex.
- At most 4 calls run at once. Calls to the same per-call host run one after another, each authenticating again; calls to a host that keeps its session share it up to its `max_channels`; a batch runs alone.
- Hardware-key and Agent signing is serialized across processes: one authentication prompt at a time; authenticated channels then run concurrently.
- Cancellation drops the server-side operation and may return `CANCELLED` if the client still receives a response. An already-started command may keep running remotely. A cancelled batch or `exec_many` may leave the caller without individual outcomes. Check the affected jobs before any retry.
- Jump chains: a hop keeps its session only when it and every hop before it keep sessions. A per-call hop makes everything behind it per-call; a jump host that keeps sessions keeps its own even when the target is per-call.

## Error codes

For `TEMPORARY_WINDOW_INCOMPATIBLE`, see [tray app restart and session lifetime](temporary-secrets.md#clear-and-lifetime).

Look up the returned [error code and recovery action](errors.md).
