# Sudo

Use `exec_sudo` on an SSH host. **Allow saved passwords for sudo** in Advanced authorizes saved-password supply, including SSH login-password reuse. Passwordless or cached authentication works with this switch off; a real password request returns `SUDO_AUTO_AUTH_DISABLED` before reading any password for that request. When allowed, ordinary sudo reuses the current SSH user's saved login password, while the required per-call `user` selects the execution identity. Changing the execution user does not change whose password ordinary sudo requests. No extra user record or duplicate input is needed for password login. The switch uses the main form's standard checkbox. Key/Agent logins or existing overrides can expand **Separate system password (optional)** when supply is enabled. Additional named passwords are in **Other account passwords (optional)**; each account password is stored once, with no authentication-account selector.

Always follow sudo's actual authentication username. An explicitly saved, nonempty matching sudo password takes precedence; otherwise only that same current SSH user's password-authenticated login password can be reused. Empty or cleared overrides fall back to the login password for that same user. Key passphrases, Agent credentials and another user's SSH password are never substituted. `rootpw`, `targetpw` and `runaspw` may request a different user's password. A read error or rejected password stops without trying a fallback password.

An empty `sudo_credentials` list does not establish that sudo is unavailable. `list_hosts.sudo_login_password_available` reports whether sudo password supply is allowed and a valid saved SSH login password is available for reuse; passwordless sudo can work even when it is false. Call `exec_sudo` for the authorized operation and judge its structured result.

```json
{"alias":"example","program":"/usr/bin/id","args":["-un"],"user":"root"}
```

Give an absolute executable path and separate argv strings. Empty arguments, whitespace and quoting are preserved. Shell scripts require an explicit interpreter, for example `program: "/bin/sh", args: ["-c", "systemctl status nginx"]`, so the script is visible to approval review. `command`, `stdin` and password arguments are rejected. There is no sudo batch tool.

The fixed launcher requires `/usr/bin/sudo`, `/bin/sh`, and sudoers permission to execute that shell as the requested user. Narrow rules permitting only the target executable may reject it. TTY requirements and additional authentication are unsupported; do not change policy, allocate a PTY, guess another password or retry automatically.

`list_hosts.sudo_enabled` reports password-supply permission, not whether remote sudo or passwordless execution is available. For `SUDO_AUTO_AUTH_DISABLED` or `SUDO_CREDENTIAL_MISSING`, open the host editor for that alias; `sudo_enabled` controls supply permission. Ordinary sudo does not require a `sudo_users` list or another authentication username. Passwords belong in the fixed login-user row in the app window. On a repeated prompt the app stops after one submission. Report other errors and let the user decide how to correct them.

The launch protocol sends READY, waits for the client's matching non-secret acknowledgment, then starts caller code with stdin closed. A bare EOF after cancellation cannot authorize that start. NOPASSWD and timestamp hits never load or send a password. After READY, target output cannot trigger password injection. Nonzero target exit codes remain `remote_error`; a failure to observe READY, a timeout or cancellation does not prove the command did not execute. Never replay an operation automatically.

`user` is required on every call, including root execution. Advanced stores authentication settings and optional passwords, not execution targets; a call omitting `user` fails validation without execution. The GUI has no dedicated sudo/su save-and-test buttons or target dialog; ordinary connection tests are unchanged.

If the target or an SSH jump host has `require_call_approval:true`, the user must approve the complete call before connection. This applies even to passwordless sudo and is independent of `sudo_enabled`. Approval does not trust a host key or override remote sudoers policy. Read [approvals.md](approvals.md); never approve your own request or automatically resubmit after a denial.

This tool handles only its fixed outer authentication phase. Ordinary `exec`/`exec_stdin` and sudo/su inside caller scripts do not gain automatic password supply. A password-looking output line is not a trusted authentication request. Do not elevate a whole mixed-permission script or replay it to compensate for an inner failure; native transparent authentication is not part of the product.
