# Su

Use `exec_su` on an SSH host. It requires an explicit `user` on each call. **Allow saved passwords for su** in Advanced uses the same standard form checkbox as other options and authorizes supply independently of sudo. Manage optional target-user passwords in the collapsed **Other account passwords (optional)** area, one account per row. Password logins need no duplicate password entry; optional system passwords for key/Agent login are also collapsed. A password belongs to an account, with separate permissions to use it for sudo and su. With this switch off, passwordless execution such as root-to-user still works; a real password request returns `SU_AUTO_AUTH_DISABLED` before reading any password for that request.

Su authenticates the **target user**. Ordinary sudo authenticates the **current SSH user** instead. Never send another user's SSH password to su. A matching saved user password is used when su requests authentication; the same target's password-authenticated SSH login password can be reused only when the usernames match exactly. Key passphrases and Agent credentials are never substituted. A read failure or rejected password stops without trying another source.

Passwords are optional. Root can normally switch to an ordinary user without a password; the remote su/PAM policy decides. If the fixed launcher becomes ready without a password prompt, the app never loads or sends any password, even if one was saved. Neither a missing record nor `has_password:false` blocks that path. Do not infer permission from numeric UID ordering or attempt to bypass remote authentication.

```json
{"alias":"example","program":"/usr/bin/id","args":["-un"],"user":"bob"}
```

Give an absolute program path and separate exact argv strings. Empty strings, quotes, Unicode and newlines are preserved without expansion; NUL is rejected. Scripts require an explicit interpreter, for example `/bin/sh` with args `["-c", "visible script"]`. There is no raw `command`, password, business `stdin`, interactive shell or batch parameter. Target programs receive stdin EOF.

The remote requires util-linux `/usr/bin/su`, `/usr/bin/env`, `/bin/sh` and support for a standard Linux-PAM C-locale `Password: ` prompt when a password is needed. The launcher selects `/bin/sh -c` without a login shell, leaves the current directory unchanged, and uses `LC_ALL=C`. It does not allocate a PTY. Restricted shells, TTY policies, other su implementations and extra PAM challenges may fail; report that result without changing policy or retrying automatically. See the upstream [su manual](https://man7.org/linux/man-pages/man1/su.1.html).

The fixed launcher emits a fresh BEGIN marker before su. Only a complete password prompt at a line boundary during that authentication phase can read the target's password, once. The fixed target shim emits READY and waits for its nonce's non-secret ACK before starting caller code. After READY, password-looking caller output cannot request credentials; the client sends ACK and EOF. A bare EOF after cancellation cannot authorize launch. Password echoes are redacted across SSH packets on both output streams.

`list_hosts.su_enabled` reports password-supply permission, not remote su availability. The existing `sudo_credentials` field is the shared account list (username and `has_password` only), retained for compatibility; an empty list does not mean su is unavailable. `exec_su` returns the usual result plus `su_user`. A nonzero caller status after READY is `remote_error`. Missing READY is `SU_START_FAILED`; timeout/cancellation cannot establish whether execution occurred. Never replay a call automatically.

For `SU_AUTO_AUTH_DISABLED` or `SU_CREDENTIAL_MISSING`, open the host editor for that alias. Non-secret prefill fields are `su_enabled` and the shared `sudo_users` list. Passwords belong in the app window. A new account may be saved with a blank password. Blank on an existing account keeps its saved password; use the actions menu to remove its separate password. Renaming allocates a new record ID and deletes the old password unless a replacement was entered. There is no dedicated GUI su test or test-target dialog; saving settings neither executes su nor stores an execution target or authentication cache.

Su errors: `SU_AUTO_AUTH_DISABLED`, `SU_INVALID_INPUT`, `SU_INVALID_PASSWORD`, `SU_CREDENTIAL_MISSING`, `SU_CREDENTIAL_READ_FAILED`, `SU_AUTH_REPEATED`, `SU_INPUT_FAILED`, `SU_START_FAILED`, `SU_CHANNEL_CLOSED`. Existing SSH, timeout and cancellation codes still apply.

Advanced stores no default execution target; a call omitting `user` fails validation without execution. `sudo -u` and `su` have different authentication and policy rules: select the mechanism for the authorized operation, without automatic fallback between them.

An SSH target or jump host with `require_call_approval:true` requires the user's whole-call approval before connection, including a passwordless root-to-user call. This is separate from `su_enabled`, password availability and remote PAM policy. Follow [approvals.md](approvals.md); no automatic resubmission or switch to sudo after rejection.

This is controlled outer su, not transparent authentication for an arbitrary script. Ordinary `exec`/`exec_stdin`, or a later `su bob` followed by `sudo` inside caller code, receives no automatic password supply. Never substitute the original SSH user's password for a changed inner identity, scan business output to decide where to send secrets, or replay a partially executed script.
