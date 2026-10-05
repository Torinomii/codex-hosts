# Hardware keys and SSH Agents

Read this when a host authenticates with a `*_sk` FIDO handle or through an Agent, when a FIDO SSH credential must be created or recovered, or when a `FIDO_*`, `SSH_AGENT_*`, `AUTH_FAILED`, or `PRIVATE_KEY_LOAD_FAILED` failure needs advice.

## Three routes

| route | profile | who signs | discovery |
|---|---|---|---|
| OpenSSH key file | `private_key` with a regular key path | codex-hosts reads the file; an encrypted file needs its passphrase in the masked field | none |
| Direct FIDO handle | `private_key` with a `*_sk` handle path | the connected hardware key through the Windows OpenSSH FIDO component (`ssh-sk-helper.exe`); no Agent, Pageant, vendor software, or service | `fido_identities` |
| SSH Agent / Pageant | `ssh_agent`, optionally `agent_key_fingerprint` | a separately running Windows OpenSSH Agent or Pageant | `agent_identities` |

- `fido_identities` scans saved handle files and reports `helper_available`; it does not prove the hardware is connected. Save a returned `path` as a `private_key` host.
- `agent_identities` lists only identities a running Agent or Pageant already exposes, at most 32. Select one by SHA-256 fingerprint and save it as `ssh_agent`; an empty fingerprint tries the exposed identities in order.
- Windows or Google passkeys are not SSH identities. Without an SSH handle, the user creates one in the GUI: the editor's **Create / recover FIDO SSH…** button opens the **FIDO SSH credential** dialog. Its recoverable option stores the credential on the key, recoverable on another supported computer through two Windows Security prompts; its compatible option writes a handle file that cannot be recovered if lost. Neither makes the hardware private key portable.
- Credentials this app creates are ECDSA-SK (what the built-in Windows WebAuthn provider supports); existing ECDSA-SK and Ed25519-SK files keep working. Never silently fall back to a different Agent key.
- Creating a credential grants no server access: the user must authorize its public key (the dialog's **Copy public key for server authorization**) on every intended server before testing.

## Prompts

- Direct FIDO and Agent signing each expect a touch or PIN. Only one authentication prompt runs at a time across processes; authenticated channels then run concurrently.
- Unless the host keeps its session, every call authenticates again: group short commands with `exec_many` and give `connect_timeout_ms` room for the prompt. On a host that keeps its session, `probe` it once at the start so the touch comes when the user expects it.
- To avoid touches entirely, the user can create the credential with OpenSSH's `no-touch-required` option and mark the key the same way in the server's `authorized_keys`; that is a key-level choice outside this app.
- The hardware private key never leaves the device; Agent forwarding stays disabled.

## Failures

- `PRIVATE_KEY_LOAD_FAILED`: the path is unreadable, the file is not an OpenSSH key or handle, or an encrypted file lacks its passphrase. Open the alias to fix the path or enter the file passphrase. A hardware PIN never goes in the passphrase field, only in the FIDO dialog or the Windows Security prompt.
- `AUTH_FAILED` on a key file: the server rejected its public key; authorize it or select another file.
- `FIDO_KEY_INVALID`: the file is not an `ecdsa-sk` or `ed25519-sk` handle. `FIDO_SIGNING_FAILED`: the Windows FIDO component is missing, or the device, PIN, or touch failed (the message says which). `FIDO_AUTH_REJECTED`: the server has not authorized the FIDO public key. For direct handles, do not suggest YubiKey Manager or enabling an Agent.
- `SSH_AGENT_UNAVAILABLE`, `SSH_AGENT_NO_KEYS`, `SSH_AGENT_KEY_NOT_FOUND`: report the Agent or Pageant state and let the user start or load their existing Agent; never start, enable, or persist an Agent service, and keep the selected fingerprint. `SSH_AGENT_SIGNING_REJECTED`: the Agent refused to sign. `SSH_AGENT_AUTH_REJECTED`: the server accepted none of the exposed identities. None of these apply to direct handles.
- `HARDWARE_AUTH_LOCK_FAILED`: the cross-process prompt lock was unavailable; report it instead of retrying in a loop.
