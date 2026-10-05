# Host editor and host-key confirmation

Read this when a host must be created, completed, or corrected, or its host key confirmed. `open_host_editor` opens a window on the user's desktop and returns only when that window closes; the user must finish it.

## Call

```json
{"alias":"example","host":"server.example.com","port":22,"user":"operator","protocol":"ssh","auth":"password","description":"staging web","tags":["staging","web"]}
```

- Omit unknown fields. There is no parameter for a password, passphrase, or PIN; the user types those in the window.
- Use the exact alias the user gave. An existing alias opens its saved profile, and the given fields overwrite those draft fields; a new alias creates a profile, saved immediately with the given fields.
- `auth` is `password`, `private-key` (alias `fido-handle`), or `ssh-agent`. In the GUI, `private-key` is **Key file / hardware key** and `ssh-agent` is **SSH Agent / Pageant**; do not present `ssh-agent` as the general hardware-key option.
- `jump_host` must be an existing verified SSH alias, the only kind the editor offers. An unknown or empty value clears the draft's jump host, so omit it to keep the saved chain.
- Omitting `description` or `tags` keeps the saved values; `description: ""` clears the notes and `tags: [""]` clears the tags. Tags are trimmed and de-duplicated ignoring case, keeping the first spelling.
- Required fields are marked `*` and highlighted if left empty on save. A new host also requires the user to choose the authentication-persistence option; do not choose or suggest one.
- **Approve each Codex call** is an SSH-only Advanced setting, default off. The user changes it in the app; `open_host_editor` has no parameter for it. One approval covers one call's hosts and commands from launch to completion, independent of authentication retention and sudo/su password-supply permissions. Saving this setting does not approve a call; the execution tool displays its own review. See [approvals.md](approvals.md).
- **Advanced** uses the same form rows and checkboxes as other options for independent sudo/su saved-password supply (`sudo_enabled` and `su_enabled`). Passwordless execution works with permission off. Password login reuses the SSH login password without duplicate login inputs. When supply is enabled, key/Agent logins or existing overrides can expand **Separate system password (optional)** for that same account. **Other account passwords (optional)** is collapsed by default and lists additional accounts with masked passwords and remove/delete menus. Do not ask the user to select a sudo authentication account.
- `sudo_users` is the compatibility name for the account-password records (`[]` removes those records; ordinary sudo can still reuse the SSH login password). It is not an authentication selector or target allowlist. Matching names keep their IDs and saved passwords. Blank keeps a saved password; renaming rotates its ID and deletes the old secret, with an optional replacement. Changing the SSH login username requires that new user's login password, or clears the old login password when saving a key/Agent login. Other named system-password records stay with their original accounts. There are no `sudo_user` / `su_user` editor fields or saved default execution targets. The editor's `user` means the SSH/Telnet login user.
- There are no dedicated **Save and test sudo/su** buttons or test-target dialog. Ordinary connection testing is unchanged. Saving authentication settings never selects an execution user or runs a privileged test. For actual operations Codex must supply `user` to `exec_sudo` / `exec_su` each time. Removing a separate system password does not disable eligible login-password reuse; use the corresponding supply switch to deny saved-password supply. See [sudo.md](sudo.md) and [su.md](su.md).
- Omit `sudo_users` unless the requested edit intentionally replaces account-password records; do not rebuild the list just to enable sudo or test another execution user. Supplying `sudo_enabled` / `su_enabled` only prefills the editor for the user to review and save; it is not execution approval.

To confirm a host key, call the same alias with the values from the `HOSTKEY_UNKNOWN` or `HOSTKEY_MISMATCH` failure:

```json
{"alias":"example","observed_fingerprint":"SHA256:…","observed_algorithm":"ssh-ed25519"}
```

The alias must exist; otherwise the window shows `HOST_NOT_FOUND` without a prompt and closing it yields `cancelled`. The window labels the fingerprint as reported by Codex so the user compares it with an out-of-band source. Accepting pins the fingerprint and algorithm; the host stays unverified until a later successful probe or test.

## While the window is open

- A still-running call is normal. Tell the user the editor is open and which fields need attention; the window may open behind Codex and only flash on the taskbar.
- If the user cannot find it, point them to the taskbar first; it opens on the desktop of the Windows user who started Codex.
- Do not open a second editor for the same alias, and do not terminate app windows or temporary-secret sessions.

## Result

- `saved`: profile and entered credentials are stored; continue the host work.
- `trusted`: the observed host key is pinned; retry the failed operation once.
- `cancelled`: the user declined or closed without saving; end that flow and report it.
- Any failure (`EDITOR_NO_RESULT`, `EDITOR_TIMEOUT`, `EDITOR_LAUNCH_FAILED`, `CANCELLED`): nothing was saved; report it. A missing result is never success.
