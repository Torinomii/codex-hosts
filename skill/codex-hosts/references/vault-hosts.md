# Bitwarden and 1Password hosts

A vault host is a Login item in the user's configured Bitwarden folder or 1Password vault. Its name or title supplies the alias, its `ssh://` or `telnet://` URI the endpoint, and its login fields the credentials. Use codex-hosts for access. The app handles `bw` and `op` and the user completes any prompts.

| Behavior | Bitwarden | 1Password |
|---|---|---|
| Discovery | Lists local mirrors. A quiet refresh can use a reusable vault session. Background listings can share a 60-second window. Refreshing the host group bypasses it. | Lists items as last read while the service is enabled. The listing itself never queries 1Password. |
| Absent alias | An operation naming the exact alias can discover a new item even within the refresh window. | An exact-alias operation or a user refresh can discover a new or renamed item. |
| Unlock | The user enters the master password in codex-hosts. If SSH approval is required, the same dialog can unlock and approve. A new-device code also belongs in the app. | codex-hosts can locate and start the installed desktop app. The user signs in, unlocks or grants access in 1Password, including Windows Hello when requested. |

Wait for the original call while the user acts. For a locked, signed-out, disabled or unconfigured service, report the code and retry only after the user confirms the issue is resolved. For a missing or mismatched item, repair it in the provider. Use `open_host_editor` only if the alias still identifies the listed vault profile. Opening an unknown alias can create a local profile. Report other [provider errors](errors.md) without repeating the unchanged call.

Each service's process-memory cache defaults to 60 idle minutes, renewed on each hit. The user can set 0–1440 minutes under **Credential services** or clear caches across running GUI/MCP processes. Zero disables caching. Bitwarden checks its unlock policy on every use and skips login-password caching under **every call**. With 1Password, external item changes or app locks may remain unseen until cache expiry or manual clearing. Its editor checks the current item for conflicts before writing.

Privileged calls can reuse the vault login password only when the chosen mechanism permits supply and requests that account's password. See [sudo](sudo.md) or [su](su.md) for the matching rules.
