# Hardware keys and SSH Agents

Read this when a host authenticates with a `*_sk` FIDO handle or through an Agent, when a FIDO SSH credential must be created or recovered, or when a `FIDO_*`, `SSH_AGENT_*`, `AUTH_FAILED`, or `PRIVATE_KEY_LOAD_FAILED` failure needs advice.

## Three distinct routes

| route | profile | who signs | discovery |
|---|---|---|---|
| OpenSSH key file | `private_key` with a regular key path | codex-hosts reads the file; an encrypted file needs its passphrase in the masked field | none |
| Direct FIDO handle | `private_key` with a `*_sk` handle path | the connected hardware key through the Windows OpenSSH FIDO component (`ssh-sk-helper.exe`); no Agent, Pageant, vendor software, or background service | `fido_identities` |
| SSH Agent / Pageant | `ssh_agent`, optionally with `agent_key_fingerprint` | a separately running Windows OpenSSH Agent or Pageant | `agent_identities` |

- `fido_identities` scans saved handle files and reports `helper_available`; it does not prove the matching hardware is connected. Save the returned `path` as a `private_key` host.
- `agent_identities` lists only identities a running Agent or Pageant already exposes, at most 32. Select by SHA-256 fingerprint and save as `ssh_agent`. Leaving the fingerprint empty tries the exposed identities in order.
- Existing Windows or Google passkeys are not SSH identities. When no SSH-specific handle exists, the user must create one in the visible GUI: the editor's **Create / recover FIDO SSH…** button opens the **FIDO SSH credential** dialog. Its recoverable option stores the credential on the key and can be recovered on another supported computer through two Windows Security prompts; its compatible option produces a handle file that cannot be recovered if lost. Neither makes the hardware private key portable.
- New credentials created by this app are ECDSA-SK because the built-in Windows WebAuthn provider supports it. Existing standard ECDSA-SK and Ed25519-SK files remain supported. Do not silently fall back to a different Agent key.
- Creating a credential grants no server access. The user must authorize the generated public key (the dialog offers **Copy public key for server authorization**) on every intended server before testing.

## Prompts and concurrency

- Direct FIDO signing and Agent signing each expect a touch or PIN prompt. Only one authentication prompt runs at a time across processes; once a session is authenticated its channels run concurrently.
- Every tool invocation is a new process that authenticates again. Group short commands with `exec_many` so one prompt serves them, and give `connect_timeout_ms` room for the prompt.
- The hardware private key never leaves the device, and Agent forwarding stays disabled.

## Failures

- `PRIVATE_KEY_LOAD_FAILED`: the key path is unreadable, the file is not an OpenSSH key or handle, or an encrypted file lacks its passphrase. Open the exact alias to correct the path or enter the file passphrase. Never put a hardware PIN in the passphrase field; the PIN belongs only to the FIDO create/recover dialog or the Windows Security prompt.
- `AUTH_FAILED`: the server rejected the key-file public key. Authorize that public key on the server or select another file.
- `FIDO_KEY_INVALID`: the selected file is not an `ecdsa-sk` or `ed25519-sk` handle. `FIDO_SIGNING_FAILED`: the Windows FIDO component is missing, or the device, PIN, or touch step failed; the message names which. `FIDO_AUTH_REJECTED`: the server has not authorized the FIDO public key. Do not suggest installing YubiKey Manager or enabling an Agent for the direct-handle route.
- `SSH_AGENT_UNAVAILABLE`, `SSH_AGENT_NO_KEYS`, `SSH_AGENT_KEY_NOT_FOUND`: report the Agent or Pageant state and let the user start or load their existing Agent; never start, enable, or persist an Agent service, and keep the selected fingerprint. `SSH_AGENT_SIGNING_REJECTED` means the Agent refused to sign; `SSH_AGENT_AUTH_REJECTED` means the server accepted none of the exposed identities. None of these apply to direct FIDO handles.
- `HARDWARE_AUTH_LOCK_FAILED`: the cross-process prompt lock could not be acquired; report it rather than retrying in a loop.
