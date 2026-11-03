# Approval and authorization dialogs

Read this when an SSH call awaits app authorization, a temporary-secret run is pending, or approval fails. These dialogs require the user's decision: explain the requested operation, but do not approve your own request, turn approval off, or switch tools/accounts to bypass a denial.

## Keep the controls distinct

| Control | Default and owner | What it authorizes |
|---|---|---|
| Codex/client tool approval | Client configuration | The client invoking a tool; separate from the app's dialog |
| **Approve each Codex call** / `require_call_approval` | Off; user changes it in the host's Advanced settings | One complete SSH tool call, including its listed hosts and commands |
| **Allow saved passwords for sudo** / **Allow saved passwords for su** | Off; independently confirmed in the host editor | Use of saved account passwords for that mechanism, not remote execution permission |
| Temporary field **Call approval** / `requires_approval` | On for a new field; user changes it in the app | One local run that references this field |
| Host-key trust and remote sudoers/PAM policy | User-confirmed pin and remote host policy | Which host is trusted, and whether native authentication/execution is allowed |

An app approval does not supply a password, trust a host key, or override sudoers/PAM. A saved password, a successful prior call, or a retained SSH connection does not authorize the next call. Passwordless sudo/su can work with password supply off but still requires call approval if that separate policy is enabled.

## SSH calls

- `list_hosts` exposes `require_call_approval` for SSH hosts. If any execution target or SSH jump host requires it, the entire call is reviewed before connecting. This includes `probe`, `batch_probe`, `exec`, `exec_stdin`, `exec_many`, `batch_exec`, `exec_sudo`, and `exec_su`; a mixed batch can also show Telnet execution targets in the same review.
- Review covers the exact hosts, commands and inputs in that call, from launch to completion. For sudo/su it shows the selected execution user; this is not necessarily the password's authentication account. It is whole-call approval, not separate authorization of each script-internal command.
- While the dialog waits, the original tool call remains pending. Tell the user which operation needs review and wait; do not submit it again or open an unrelated host editor to approve it. The app may appear behind the client; point the user to its taskbar window if needed.
- The blue overview has red **Allow all** and white **Reject all** buttons for the frozen batch currently displayed. Later arrivals are not included; the user selects **View next batch**. Do not ask for blanket approval of unrelated calls.
- Clicking a command or **View full details** opens the full call: execution targets and jump hosts are distinct; commands and stdin retain their exact text. Long text is paged with **Copy full text**, not silently truncated. Detail buttons decide that entire call, not only the clicked command.
- **Back** or Escape in details returns to the overview without deciding. Escape in the overview rejects only the displayed batch, not later arrivals. Clicking outside the modal does not approve anything. View transitions briefly disable actions to avoid carrying a click to a new request.
- When the call reads a Bitwarden host and the vault is locked, the same dialog asks for the master password and its button reads **Unlock and allow all** (or **Unlock and allow this call**). The calls are allowed only after the vault opens; a wrong password keeps them waiting with the reason shown. Without call approval, the app shows its own unlock prompt instead.
- When the call reads a 1Password host, the 1Password app may ask the user to allow codex-hosts after the call is allowed; that prompt belongs to 1Password, and declining it fails the call with `ONEPASSWORD_LOCKED`.
- Waiting requests expire after five minutes; the queue holds at most 16 review records. Decisions are single-use and never carried to another call. Cancellation removes the pending request; expiry/cancellation invalidates open details. Feedback saying Allowed/Rejected is an authorization decision, not an execution result.

## Temporary-secret runs

- New fields require approval. `temporary_secrets_open` / `_status` exposes readiness and `requires_approval`, never values. If any field named by the run's `env` mapping requires approval, that run waits for the user's decision. Tools cannot change field approval settings.
- The blue authorization popup is inside the temporary-secret editor. Its top red **Allow this run** and white **Reject this run** buttons decide only that operation, not every waiting local operation. The summary shows distinct field count, pending count and the five-minute deadline.
- Command details show program, JSON argv, working directory, execution timeout and environment-variable-to-field mappings. Secrets are never shown. Back/Escape in details makes no decision; Escape in the overview rejects the displayed run. SSH review takes precedence if both kinds are pending.
- Only one local child runs at a time. While another runs, Allow is disabled and Stop is available. If all referenced fields have approval off, the run starts directly or waits as `queued`, then starts automatically. See [temporary-secrets.md](temporary-secrets.md) for child isolation and lifetime limits.
- `temporary_secrets_run` returns an `operation_id`; poll that session's `_status` for that operation. `saved`, Allowed, `pending`, `queued`, and `running` are not success. Only `completed` with exit code 0 means successful completion.
- Saving/replacing/clearing a field or changing its approval switch revokes pending and queued local runs; an already-running child keeps its original environment. Reopening/clearing a field preserves its approval choice within that vault session; a new field defaults on.

## Failures and retries

| Result | Agent action |
|---|---|
| `APPROVAL_CANCELLED` | The SSH call was not authorized. Report the rejection/cancellation and stop; do not resubmit or bypass it. |
| `APPROVAL_EXPIRED` | The SSH review expired before execution. Ask whether the user wants a new call; never re-prompt automatically. |
| `APPROVAL_UNAVAILABLE` | The GUI/IPC may be unavailable, the running holder incompatible, or the review queue full. Report the exact result and resolve the app state with the user; do not disable policy or treat this as a password error. |
| Local run `cancelled` or `expired` | Report the terminal operation state; do not create a replacement run automatically. |
| Missing response, timeout, or client cancellation | Do not infer no execution. SSH has no resumable approval handle. For a local run, reconcile through its session's operations when possible; never blindly resubmit. |

The three `APPROVAL_*` failures stop before SSH connection/command execution for that call. After approval, normal host-key, credential, connection, sudo/su, and command failures can still occur. Always wait for the structured execution result and inspect `status` and `exit_code`; neither permission feedback nor a hidden/closed dialog proves success.
