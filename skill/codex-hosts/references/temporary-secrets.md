## Temporary project secrets (memory only)

Use this in-app editor for API keys, tokens and secret parameters unrelated to host login.
Check `capabilities` for `temporary_secrets_open` first; do not assume an older installed binary supports it.
No host listing or host profile is needed for these actions. IPC is restricted to the same Windows user;
if a sandbox uses a different user SID, use the approved interactive-user execution context rather than
weakening pipe permissions.

1. Derive detailed, non-secret names from the current project/environment/service/purpose, e.g.
   `myproject-local-qwen-apikey` or `myproject-staging-payments-signing-key`. Reuse exact names within
   the active main-process session; never use the key value as the name. Use distinct project/environment prefixes because normal app launches share one in-memory vault.
2. Invoke `{"action":"temporary_secrets_open","fields":["myproject-local-qwen-apikey"]}` through the
   normal no-secret tool request/result workflow. It opens/restores the main app and its internal editor and returns
   an opaque `session` UUID plus readiness metadata. The helper tool exits; the main app owns the vault. Repeated open calls reuse its session.
3. Inspect readiness for the exact required names in the returned session. If all are already
   `ready:true` and status is `ok` or `saved`, reuse them without asking for re-entry or another Save.
   For missing fields, ask the user to fill only those right-hand masked cells and click Save, never
   paste values into chat. Poll `{"action":"temporary_secrets","session":"<UUID>","request":{"action":"status"}}`
   only as needed. A successful Save reports `saved`; verify every required name is present and ready
   before execution. Another task's Save is not proof that your missing fields have been filled.
   `saved` acknowledges input, not session-wide authorization: declaring fields or executing resets it
   to `ok` without deleting saved values. Readiness, not a new `saved` event, permits later reuse.
   Neither signal proves authentication or command success; every execution still needs approval.
4. Add fields using the same outer request and inner `{"action":"request","fields":["..."]}`.
   Existing saved fields are preserved. Up to 64 names of 256 bytes; values up to 32 KiB, including
   whitespace/newlines (NUL unsupported). Save replaces a value; an empty editor after Save is normal.
5. For use, request inner `{"action":"execute","program":"C:\\absolute\\python.exe",
   "args":["C:\\project\\trusted_script.py"],"cwd":"C:\\project",
   "env":{"QWEN_API_KEY":"myproject-local-qwen-apikey"},"timeout_ms":60000}`.
   Use only trusted project programs and non-secret arguments. It returns `operation_id` immediately.
   The popup shows the exact executable, arguments, directory and mappings for explicit user approval.
   Poll status for this ID; only `completed` with exit code 0 is success. Pending approvals expire in
   five minutes. At most one child runs, timeout is 1–600000 ms, and the last 16 operations are retained.
6. Never request plaintext read/export, echo a secret, write it into `.env` or other project files,
   or reconstruct it from process memory/logs. Values are injected directly into the selected child's
   environment, never shell-substituted into arguments. stdin/stdout/stderr are discarded and no
   child output is returned. The receiving program is trusted, not sandboxed: it can write files,
   make network calls, or spawn descendants. Stop/timeout terminates only the direct child.
7. Inner `{"action":"clear","fields":["name"]}` clears named values; an empty list clears all.
   Saving/replacing/clearing cancels pending approvals; a running child already owns its environment.
   Closing the editor hides it without clearing. Closing the main window hides to the tray and keeps
   the process, IPC and values alive. Relaunching the executable or requesting fields restores the same
   session. Only tray Exit, process termination, sign-out or system restart loses values
   and invalidates references. Do not silently substitute an expired session. No Credential Manager
   or startup service. Existing --codex-edit callback windows still close/exit normally.

Connection retries apply only before a request is sent when the pipe is busy, within a five-second
total deadline. On an execute call with a missing response, do not resubmit automatically: poll the
same session's operations to reconcile its outcome, or ask the user if it cannot be determined.
