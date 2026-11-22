# Temporary project secrets (memory only)

Use `temporary_secrets_open`, `temporary_secrets_status`, and `temporary_secrets_run` for API keys, tokens, and other secret parameters unrelated to host login. These tools work without a host profile.

## Rules

- Never request a plaintext read or export, echo a value, write it into `.env` or any other file, or reconstruct it from process memory or logs. Values reach only the selected child's environment, never shell-substituted into arguments.
- The window-wide **Call approval** setting starts on. Changing it applies to every existing field and sets the default for future fields. The app saves that setting across restarts and asks the user to confirm before turning it off. A dash means at least one field differs from the saved default. Only the user changes these settings. A run requires approval if any field in its `env` mapping reports `requires_approval:true`.
- These local-run approvals are distinct from a host's `require_call_approval` and sudo/su supply switches. Let the user decide the popup; never approve your own request or change policy to bypass it. Full dialog behavior and failure handling: [approvals.md](approvals.md).
- The vault's IPC is restricted to the same Windows user. If a sandbox runs under a different user SID, use the approved interactive-user execution context; never weaken the pipe's permissions.
- Never silently substitute a different or expired session.

## Fields

1. Name each field in detail from project, environment, service, and purpose, such as `myproject-local-qwen-apikey` or `myproject-staging-payments-signing-key`. A name is not secret and never the value. Normal app launches share one vault, so use distinct project and environment prefixes, and reuse exact names within the running session.
2. `temporary_secrets_open` with `{"fields":["myproject-local-qwen-apikey"]}` opens or restores the main app and its editor and returns an opaque `session` UUID with per-field readiness. Repeated calls reuse the session; calling it with new names adds fields and keeps saved ones. Limits: 64 names of up to 256 bytes; values up to 32 KiB, whitespace and newlines included, no NUL.
3. If every required name is `ready:true` and `status` is `ok` or `saved`, reuse the saved values. For each missing value, ask the user to fill its masked input in the app and select **Save**. Narrow windows stack the controls under each field name. Check readiness for every field the run needs with `temporary_secrets_status`.
4. `saved` acknowledges input, not authorization; declaring fields or running resets `status` to `ok` without deleting values. Readiness, not a new `saved`, allows reuse. Neither proves that a credential works or a run succeeded. Save replaces a value, and an empty editor after Save is normal.
5. Open/status reports each field's `requires_approval`. Reopening a field preserves the user's choice. The editor's bottom bar shows the running operation or latest result, waiting count and saved field count. Select **History** there to expand the operation list.

## Run

```json
{"session":"<UUID>","program":"C:\\absolute\\python.exe","args":["C:\\project\\trusted_script.py"],"cwd":"C:\\project","env":{"QWEN_API_KEY":"myproject-local-qwen-apikey"},"timeout_ms":60000}
```

- Run only trusted project programs, with non-secret arguments; `env` maps variable names to field names. When approval is required, a blue popup nested inside the temporary-secret editor shows a run summary, waiting count, and five-minute approval countdown. Its top red Allow this run button approves only this operation; its white Reject this run button or Escape rejects it.
- Click the command preview or View full details to open complete local execution details: program, JSON argument list, directory, execution timeout, and field mappings without secret values. Long text is paged without truncation and has Copy full text. The detail window's top buttons also decide only this operation; Back or Escape returns without making a decision. Expired or revoked details cannot authorize another run.
- While another child runs, Allow is disabled with an explanation and Stop is available. The history above the bottom bar lists up to 16 operations, newest first, and scrolls independently of the fields.
- The call returns an `operation_id`. Poll `temporary_secrets_status` with the same `session` and find the entry in `operations` whose `id` matches. Success requires `status:"completed"` and `exit_code:0` on that entry. One child runs at a time. An approval-free run waits as `queued` while another child runs, then starts automatically. The default timeout is 60,000 ms and `timeout_ms` accepts 1–600,000 ms.
- Neither `saved` input nor an Allowed message means execution succeeded. `pending`, `queued`, and `running` are not completion. Rejection or expiry is final for that operation; do not resubmit automatically. Local popup buttons decide only the displayed run; the SSH dialog's frozen-batch Allow all behavior does not authorize every waiting local run.
- The child's stdin, stdout, and stderr are discarded; no output is returned. It is trusted, not sandboxed: it can write files, use the network, and spawn processes. Stop or timeout ends only the direct child.
- If an execute call returns no response, do not resubmit: reconcile through that session's operations in `temporary_secrets_status`, or ask the user. The client itself retries only while the pipe is busy before the request is sent, within five seconds.

## Clear and lifetime

- `TEMPORARY_WINDOW_INCOMPATIBLE` means the running tray app cannot confirm that only the user can clear values. The requested operation did not run. Let the user choose when to discard existing values, exit from the system tray, and reopen the updated app. Opening a window or restarting MCP alone leaves the holder running.
- Saved values are reusable across runs and tasks for the lifetime of the tray app. Only the user can clear a value in the temporary secrets window. **Clear** removes that field's saved value and draft and keeps its name and approval setting. Older clients that try to clear values through IPC receive `SECRET_CLEAR_UI_ONLY`. Saving, replacing, clearing, or changing an approval checkbox cancels pending and queued operations. A running child keeps the environment it started with.
- After confirmation, **Delete all** removes every field, saved value and draft. It cancels waiting operations and lets running programs finish. If the field list changes during confirmation, the app asks the user to review it again. Declaring a deleted name creates an empty field that inherits the current window-wide approval setting.
- The main (tray) app holds the vault, independent of the MCP server, which starting or stopping with Codex does not affect. The server starts the app outside its own job object; if the job forbids that, the result carries a `warning` that the app will close with Codex, and the user should start the codex-hosts GUI by hand first.
- Closing the editor or the main window only hides it; the process, IPC, and values stay, and relaunching or requesting fields restores the same session. Only tray Exit, process termination, sign-out, or restart loses the values and invalidates references. Nothing is written to Credential Manager, and no startup entry or service is added. Host-editor windows opened for Codex still close normally.
