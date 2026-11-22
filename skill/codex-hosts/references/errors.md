# Errors and recovery

Read the code for the affected call or host. If execution is uncertain, inspect remote state read-only before discussing a retry with the user. Inspect each batch or `exec_many` result and preserve completed work. Report other failures without repeating the same call.

## Host selection, login and trust

Request validation: `REQUEST_INVALID`, `ALIAS_INVALID`, `ALIAS_NOT_FOUND`, `HOST_HIDDEN` (hidden from Codex in its profile), `PROFILE_INVALID` (saved host incomplete; open the editor), `COMMANDS_REQUIRED`, `TOO_MANY_COMMANDS`, `COMMAND_INVALID`, `BATCH_ALIASES_REQUIRED`, `BATCH_TOO_LARGE`, `BATCH_ALIAS_INVALID`, `STDIN_TOO_LARGE`, `STDIN_UNSUPPORTED` (stdin on a Telnet host), `SSH_REQUIRED` (`exec_many` on a Telnet host), `STORE_READ_FAILED`, `STORE_WRITE_FAILED`.

- For `ALIAS_NOT_FOUND` after an exact-alias operation, open the [host editor](host-editor.md) only if local creation is intended. For `PROFILE_INVALID`, open the affected profile. Respect `HOST_HIDDEN`.
- On `CREDENTIAL_MISSING` during login, let the user enter the password in the editor. Retry the affected pre-execution failure once after `saved`. `CREDENTIAL_READ_FAILED` calls for checking the MCP process's Windows account and Credential Manager access. A false `has_required_secret` alone cannot distinguish them.
- On `HOSTKEY_UNKNOWN` or `HOSTKEY_MISMATCH`, open the editor for the failure's `host_alias` with its `observed_fingerprint` and `observed_algorithm`. Retry only the affected unstarted operation, once after `trusted`.
- For `CONNECT_TIMEOUT` or `AUTH_TIMEOUT`, report the failed hop and phase. For key-file, FIDO or Agent authentication, use [key diagnostics](hardware-keys.md).

Connection and host keys: `CONNECT_FAILED`, `CONNECT_TIMEOUT`, `HANDSHAKE_FAILED`, `HOSTKEY_MISSING`, `HOSTKEY_UNKNOWN`, `HOSTKEY_MISMATCH`, `INVALID_HOST_CHAIN` (a hop is missing, not SSH, cyclic, or deeper than eight), `JUMP_CHANNEL_FAILED`, `SSH_CONNECTION_POOL_FULL`, `SSH_POOL_FAILED`, `SSH_BATCH_RECONNECT_BLOCKED`.

Authentication: `AUTH_TIMEOUT`, `CREDENTIAL_MISSING`, `CREDENTIAL_READ_FAILED`, `AUTH_FAILED` (also when a keyboard-interactive server asks for more than one password, such as a one-time code, which is never answered), `PRIVATE_KEY_LOAD_FAILED`, `FIDO_KEY_INVALID`, `FIDO_SIGNING_FAILED`, `FIDO_AUTH_REJECTED`, `HARDWARE_AUTH_LOCK_FAILED`, `SSH_AGENT_UNAVAILABLE`, `SSH_AGENT_NO_KEYS`, `SSH_AGENT_KEY_NOT_FOUND`, `SSH_AGENT_AUTH_FAILED`, `SSH_AGENT_SIGNING_REJECTED`, `SSH_AGENT_AUTH_REJECTED`, `SSH_AGENT_TIMEOUT` (identity discovery over five seconds).

## Execution and approvals

SSH call approval can fail with `APPROVAL_CANCELLED`, `APPROVAL_EXPIRED` (five minutes without a decision) or `APPROVAL_UNAVAILABLE` (GUI, IPC or review queue unavailable). These stop the call before remote execution. For `APPROVAL_TARGET_CHANGED`, the approved endpoint or login changed and execution stopped for the affected target. Inspect each host's result before any retry. Other hosts may have completed. Verify the updated destination and submit only affected, unstarted work for a new review. See [approval failures](approvals.md#failures-and-retries).

Execution: `CHANNEL_OPEN_FAILED`, `REMOTE_EXEC_FAILED` (the server rejected the exec request), `STDIN_WRITE_FAILED` (the channel closed before all input was sent and no exit status arrived; the command may have partly run), `COMMAND_TIMEOUT`, `OPERATION_TIMEOUT`, `CANCELLED`, `COMMAND_CANCELLED`, `COMMAND_WORKER_FAILED`, `BATCH_TIMEOUT`, `BATCH_CANCELLED`, `BATCH_INTERNAL_FAILED`, `BATCH_RESULT_TOO_LARGE`, `EXEC_MANY_RESULT_TOO_LARGE`, `SERIALIZE_FAILED`.

`BATCH_TIMEOUT`, `COMMAND_TIMEOUT`, `OPERATION_TIMEOUT`, `CANCELLED`, `STDIN_WRITE_FAILED` or a lost response may leave work running or partly completed. Check the affected jobs. `SSH_BATCH_RECONNECT_BLOCKED` stops the affected work. Report those hosts without restarting the batch.

Telnet: `AUTH_FAILED` (the server printed a login failure), `TELNET_CLOSED`, `TELNET_READ_FAILED`, `TELNET_WRITE_FAILED`, `OUTPUT_LIMIT` (Telnet output over 1 MiB fails instead of being truncated), and `COMMAND_TIMEOUT` when a prompt never matches.

## Privileged execution

Sudo: `SUDO_AUTO_AUTH_DISABLED`, `SUDO_INVALID_INPUT`, `SUDO_INVALID_PASSWORD`, `SUDO_CREDENTIAL_MISSING`, `SUDO_CREDENTIAL_READ_FAILED`, `SUDO_AUTH_REPEATED` (no second password), `SUDO_PROTOCOL_FAILED`, `SUDO_INPUT_FAILED`, `SUDO_START_FAILED`, `SUDO_CHANNEL_CLOSED`; existing connection, timeout and cancellation codes still apply. `SSH_REQUIRED` also covers sudo on Telnet.

Su: `SU_AUTO_AUTH_DISABLED`, `SU_INVALID_INPUT`, `SU_INVALID_PASSWORD`, `SU_CREDENTIAL_MISSING`, `SU_CREDENTIAL_READ_FAILED`, `SU_AUTH_REPEATED`, `SU_INPUT_FAILED`, `SU_START_FAILED`, `SU_CHANNEL_CLOSED`; existing SSH, timeout and cancellation codes still apply.

Execution targets are not host settings. Missing or null `user` is rejected before execution as invalid arguments. Empty/invalid targets return `SUDO_INVALID_INPUT` / `SU_INVALID_INPUT`; internal launches without a target fail with `SUDO_USER_REQUIRED` / `SU_USER_REQUIRED`. The results' `sudo_user` / `su_user` identify the explicit target. The GUI has no dedicated sudo/su test or target dialog.

For disabled supply or a missing account password, open the affected alias in the editor. An editor save alone does not authorize replaying a privileged call. Stop after a rejected password, repeated prompt, credential read or start failure. See [sudo](sudo.md) or [su](su.md).

## Editor, local project secrets and vault hosts

On `TEMPORARY_WINDOW_INCOMPATIBLE`, follow the [tray restart procedure](temporary-secrets.md#clear-and-lifetime).

Editor and secret errors include `EDITOR_ALIAS_REQUIRED`, `EDITOR_LAUNCH_FAILED`, `EDITOR_TIMEOUT` (after an hour the window can still save), `EDITOR_WAIT_FAILED`, `EDITOR_NO_RESULT` (no save confirmation reached the caller), `EDITOR_RESULT_INVALID`, `TEMPORARY_WINDOW_FAILED`, `TEMPORARY_WINDOW_TIMEOUT`, `TEMPORARY_SESSION_UNAVAILABLE`, and `PLATFORM_UNSUPPORTED`. Local project-secret errors with `status:"error"` and `isError` include `SECRET_MISSING`, `FIELD_NOT_FOUND`, `FIELD_LIMIT`, `FIELD_NAMES_INVALID`, `SECRET_VALUE_INVALID`, `OPERATION_LIMIT`, `EXECUTION_INVALID`, `ENVIRONMENT_INVALID`, `TEMPORARY_BUSY`, and `TEMPORARY_REQUEST_INVALID`.

Bitwarden hosts: `BITWARDEN_LOCKED` (the master-password prompt was cancelled or expired; no password was read), `BITWARDEN_SIGNED_OUT`, `BITWARDEN_DISABLED`, `BITWARDEN_NOT_CONFIGURED`, `BITWARDEN_ITEM_MISSING` (removed, in the trash, or without a password), `BITWARDEN_ITEM_MISMATCH` (no longer a host item, or its username changed), `BITWARDEN_SYNC_REQUIRED`, `BITWARDEN_UNREACHABLE`, `BITWARDEN_VERIFICATION_REQUIRED`, `BITWARDEN_INVALID_CREDENTIALS`, `BITWARDEN_CLI_UNSUPPORTED`, `BITWARDEN_UNAVAILABLE`. The user resolves them in the app window or in Bitwarden.

1Password hosts: `ONEPASSWORD_LOCKED` (the 1Password app's prompt was declined or not answered; nothing was read), `ONEPASSWORD_UNAVAILABLE` (the app is not running or not reachable, or the CLI failed), `ONEPASSWORD_NOT_CONFIGURED` (no op.exe, account, or vault chosen, or the app's CLI integration is off), `ONEPASSWORD_CLI_UNSUPPORTED`, `ONEPASSWORD_DISABLED`, `ONEPASSWORD_ITEM_MISSING` (removed, or without a password), `ONEPASSWORD_ITEM_MISMATCH` (no longer a host item, or its username changed). The user resolves these in 1Password or the codex-hosts window. For an item problem, use `open_host_editor` only while the alias still identifies the listed vault profile.

For a delayed or missing save callback, see [host editor outcomes](host-editor.md#result). For field or runner failures, see [temporary project secrets](temporary-secrets.md). For provider prompts and recovery, see [vault hosts](vault-hosts.md).
