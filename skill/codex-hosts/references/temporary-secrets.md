# Temporary project secrets (memory only)

Use this for API keys, tokens, and secret parameters unrelated to host login; no host profile is needed. Tools: `temporary_secrets_open`, `temporary_secrets_status`, `temporary_secrets_run`, `temporary_secrets_clear`.

## Rules

- Never request a plaintext read or export, echo a value, write it into `.env` or any other file, or reconstruct it from process memory or logs. Values reach only the selected child's environment, never shell-substituted into arguments.
- Each field's Call approval switch defaults on. A `temporary_secrets_run` requires the user's popup approval if any field referenced by `env` has `requires_approval:true`. All referenced fields switched off allows direct execution. Only the user changes these switches in the app; tools cannot disable approval.
- These local-run approvals are distinct from a host's `require_call_approval` and sudo/su supply switches. Let the user decide the popup; never approve your own request or change policy to bypass it. Full dialog behavior and failure handling: [approvals.md](approvals.md).
- The vault's IPC is restricted to the same Windows user. If a sandbox runs under a different user SID, use the approved interactive-user execution context; never weaken the pipe's permissions.
- Never silently substitute a different or expired session.

## Fields

1. Name each field in detail from project, environment, service, and purpose, such as `myproject-local-qwen-apikey` or `myproject-staging-payments-signing-key`. A name is not secret and never the value. Normal app launches share one vault, so use distinct project and environment prefixes, and reuse exact names within the running session.
2. `temporary_secrets_open` with `{"fields":["myproject-local-qwen-apikey"]}` opens or restores the main app and its editor and returns an opaque `session` UUID with per-field readiness. Repeated calls reuse the session; calling it with new names adds fields and keeps saved ones. Limits: 64 names of up to 256 bytes; values up to 32 KiB, whitespace and newlines included, no NUL.
3. If every required name is `ready:true` and `status` is `ok` or `saved`, reuse them without asking for re-entry. Otherwise ask the user to fill only the missing fields in the right-hand masked cells and click Save, never to paste values into chat, and poll `temporary_secrets_status` only as needed. Before running, confirm every required name is ready; another task's Save proves nothing about your fields.
4. `saved` acknowledges input, not authorization; declaring fields or running resets `status` to `ok` without deleting values. Readiness, not a new `saved`, allows reuse. Neither proves that a credential works or a run succeeded. Save replaces a value, and an empty editor after Save is normal.
5. Field name, masked secret value and Call approval share aligned columns; narrow windows stack them. Approval uses the same standard checkbox as the main interface. Open/status metadata reports each field's `requires_approval` without its value. Reopening or clearing a value preserves its approval setting within the vault session; new fields default on.

## Run

```json
{"session":"<UUID>","program":"C:\\absolute\\python.exe","args":["C:\\project\\trusted_script.py"],"cwd":"C:\\project","env":{"QWEN_API_KEY":"myproject-local-qwen-apikey"},"timeout_ms":60000}
```

- Run only trusted project programs, with non-secret arguments; `env` maps variable names to field names. When approval is required, a blue popup nested inside the temporary-secret editor shows a run summary, waiting count, and five-minute approval countdown. Its top red Allow this run button approves only this operation; its white Reject this run button or Escape rejects it.
- Click the command preview or View full details to open complete local execution details: program, JSON argument list, directory, execution timeout, and field mappings without secret values. Long text is paged without truncation and has Copy full text. The detail window's top buttons also decide only this operation; Back or Escape returns without making a decision. Expired or revoked details cannot authorize another run.
- While another child runs, Allow is disabled with an explanation and Stop is available. Permission feedback is distinct from the actual execution result; active operations and the four most recent outcomes remain visible in the editor.
- The call returns an `operation_id` at once; poll `temporary_secrets_status` for it. Only `completed` with exit code 0 is success. One child runs at a time; an approval-free call waits as `queued` if another child is running and then starts automatically. `timeout_ms` is 1–600000, and at most 16 active/recent operations are kept.
- Neither `saved` input nor an Allowed message means execution succeeded. `pending`, `queued`, and `running` are not completion. Rejection or expiry is final for that operation; do not resubmit automatically. Local popup buttons decide only the displayed run; the SSH dialog's frozen-batch Allow all behavior does not authorize every waiting local run.
- The child's stdin, stdout, and stderr are discarded; no output is returned. It is trusted, not sandboxed: it can write files, use the network, and spawn processes. Stop or timeout ends only the direct child.
- If an execute call returns no response, do not resubmit: reconcile through that session's operations in `temporary_secrets_status`, or ask the user. The client itself retries only while the pipe is busy before the request is sent, within five seconds.

## Clear and lifetime

- `temporary_secrets_clear` with `fields` clears those values; an empty list clears all values while retaining the fields and their approval settings. Saving, replacing, clearing, or changing an approval checkbox cancels pending and queued operations; a running child keeps the environment it started with.
- The GUI's **Delete all** is different: after confirmation it removes every field, saved value and unsaved draft, and cancels waiting operations without stopping running programs. If the field list changes during confirmation, the user must review it and confirm again. Declaring a deleted name creates an empty field with approval enabled; never assume a deleted value can be reused.
- The main (tray) app holds the vault, independent of the MCP server, which starting or stopping with Codex does not affect. The server starts the app outside its own job object; if the job forbids that, the result carries a `warning` that the app will close with Codex, and the user should start the codex-hosts GUI by hand first.
- Closing the editor or the main window only hides it; the process, IPC, and values stay, and relaunching or requesting fields restores the same session. Only tray Exit, process termination, sign-out, or restart loses the values and invalidates references. Nothing is written to Credential Manager, and no startup entry or service is added. Host-editor windows opened for Codex still close normally.
