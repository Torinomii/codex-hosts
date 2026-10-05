# Sudo

Use `exec_sudo` on an SSH host. **Allow sudo passwords** in Advanced authorizes saved-password supply, including SSH login-password reuse. Passwordless or cached authentication works with this switch off; a real password request returns `SUDO_AUTO_AUTH_DISABLED` before reading any password for that request. When allowed, ordinary sudo reuses the current SSH user's saved login password, while the required per-call `user` selects the execution identity. Changing the execution user does not change whose password ordinary sudo requests. No extra user record is needed for a password-authenticated SSH login. The separate sudo section displays the login username read-only and lets key/Agent logins supply that same user's system password in one masked row. It has no authentication-account selector or account list. Other named passwords appear in the su section; the same account password is stored only once.

Always follow sudo's actual authentication username. An explicitly saved, nonempty matching sudo password takes precedence; otherwise only that same current SSH user's password-authenticated login password can be reused. Empty or cleared overrides fall back to the login password for that same user. Key passphrases, Agent credentials and another user's SSH password are never substituted. `rootpw`, `targetpw` and `runaspw` may request a different user's password. A read error or rejected password stops without trying a fallback password.

An empty `sudo_credentials` list does not establish that sudo is unavailable. `list_hosts.sudo_login_password_available` reports whether sudo password supply is allowed and a valid saved SSH login password is available for reuse; passwordless sudo can work even when it is false. Call `exec_sudo` for the authorized operation and judge its structured result.

```json
{"alias":"example","program":"/usr/bin/id","args":["-un"],"user":"root"}
```

Give an absolute executable path and separate argv strings. Empty arguments, whitespace and quoting are preserved. Shell scripts require an explicit interpreter, for example `program: "/bin/sh", args: ["-c", "systemctl status nginx"]`, so the script is visible to approval review. `command`, `stdin` and password arguments are rejected. There is no sudo batch tool.

The fixed launcher requires `/usr/bin/sudo`, `/bin/sh`, and sudoers permission to execute that shell as the requested user. Narrow rules permitting only the target executable may reject it. TTY requirements and additional authentication are unsupported; do not change policy, allocate a PTY, guess another password or retry automatically.

`list_hosts.sudo_enabled` reports password-supply permission, not whether remote sudo or passwordless execution is available. For `SUDO_AUTO_AUTH_DISABLED` or `SUDO_CREDENTIAL_MISSING`, open the host editor for that alias; `sudo_enabled` controls supply permission. Ordinary sudo does not require a `sudo_users` list or another authentication username. Passwords belong in the fixed login-user row in the app window. On a repeated prompt the app stops after one submission. Report other errors and let the user decide how to correct them.

The launch protocol sends READY, waits for the client's matching non-secret acknowledgment, then starts caller code with stdin closed. A bare EOF after cancellation cannot authorize that start. NOPASSWD and timestamp hits never load or send a password. After READY, target output cannot trigger password injection. Nonzero target exit codes remain `remote_error`; a failure to observe READY, a timeout or cancellation does not prove the command did not execute. Never replay an operation automatically.

`user` is required on every call, including root execution. Advanced stores authentication settings and optional passwords, not execution targets. Existing JSON `default_user` values are ignored and omitted on save; existing credential IDs and secret slots remain. Older calls omitting `user` now fail validation without execution. GUI **Save and test sudo** asks for a temporary target, initially root, and runs `/usr/bin/id`; cancelling saves and executes nothing. The test target is not stored in the host profile.
