# Host editor and host-key confirmation

Read this when a host must be created, completed, corrected, or its host key confirmed. `open_host_editor` opens a visible window on the user's desktop and returns only when it closes, so treat the call as an interactive step that the user must finish.

## Call

```json
{"alias":"example","host":"server.example.com","port":22,"user":"operator","protocol":"ssh","auth":"password","description":"staging web","tags":["staging","web"]}
```

- Omit unknown fields. There is no parameter for a password, passphrase, or PIN; the user enters those in the window.
- Use the exact alias the user gave. When the alias exists, its saved profile opens and the given fields overwrite the matching draft fields; when it does not exist, a new profile is created and saved immediately with the given fields.
- `auth` accepts `password`, `private-key` (also `fido-handle`), and `ssh-agent`. The stable names differ from the GUI labels: `private-key` is **Key file / hardware key** and `ssh-agent` is **SSH Agent / Pageant**. Do not describe `ssh-agent` as the general hardware-key option.
- `jump_host` should name an existing verified SSH alias, the only kind the editor offers. An unknown or empty value clears the draft's jump host, so omit it when the chain should stay as saved.
- Metadata prefill: omitting `description` or `tags` keeps the saved values, `description: ""` clears the notes, and `tags: [""]` clears the tags. Tags are trimmed and de-duplicated ignoring case, keeping the first spelling.
- The editor marks required fields with `*` and, if the user tries to save with one empty, highlights it. A new host also requires the user to choose the authentication-persistence option; do not pre-empt that choice or suggest one.

For host-key confirmation, call the same alias with the values from the `HOSTKEY_UNKNOWN` or `HOSTKEY_MISMATCH` failure:

```json
{"alias":"example","observed_fingerprint":"SHA256:…","observed_algorithm":"ssh-ed25519"}
```

The alias must already exist; otherwise the window shows `HOST_NOT_FOUND`, no prompt appears, and closing it yields `cancelled`. The window labels the fingerprint as reported by Codex so the user compares it with an out-of-band source. Accepting stores the new fingerprint and algorithm but leaves the host unverified until a later successful probe or test.

## While the window is open

- The call stays running until the user closes the window; a tool call that is still running is normal here. Tell the user the editor is open and which fields need attention. The window may open behind Codex and flash on the taskbar.
- Do not start a second editor for the same alias while one is open, and do not terminate app windows or temporary-secret sessions.
- If the user cannot find a window, ask them to check the taskbar before anything else; the server runs as the user who started Codex, and the window appears on that user's desktop.

## Read the result

- `saved`: the profile and any entered credentials are stored; continue with the host work.
- `trusted`: the observed host key is now pinned; retry the failed operation once.
- `cancelled`: the user declined, or the window closed without saving; end that flow and report it.
- Any failure (`EDITOR_NO_RESULT`, `EDITOR_TIMEOUT`, `EDITOR_LAUNCH_FAILED`, `CANCELLED`): nothing was saved; report it. Never treat a missing result as success.
