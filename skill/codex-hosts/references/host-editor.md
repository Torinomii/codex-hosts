# Host editor and host-key confirmation

Read this when a host must be created, completed, corrected, or its host key confirmed. Both flows open a visible window and report through a JSON result file, so treat them as interactive steps that the user must finish.

## Launch

```text
--codex-edit --alias <alias> --host <host> --port <port> --user <user> --protocol <ssh|telnet> --auth <password|private-key|ssh-agent> --key-path <path> --agent-key-fingerprint <SHA256:...> --jump-host <alias> --description <notes> --tag <tag> --tag <tag> --result-file <result.json>
```

- Omit unknown fields. Never add a password, passphrase, or PIN argument; the editor has no such flags and the process rejects unknown arguments with exit code 2.
- Use the exact alias the user gave. When the alias exists, its saved profile opens and the given fields overwrite the matching draft fields; when it does not exist, a new profile is created and saved immediately with the given fields. Without `--alias` the app picks a neutral `host-N` name.
- `--auth` accepts `password`, `private-key` (also `fido-handle`), and `ssh-agent`. The stable names differ from the GUI labels: `private-key` is **Key file / hardware key** and `ssh-agent` is **SSH Agent / Pageant**. Do not describe `ssh-agent` as the general hardware-key option.
- `--jump-host` should name an existing verified SSH alias, the only kind the editor offers. An unknown or empty value clears the draft's jump host, so omit the flag when the chain should stay as saved.
- Metadata prefill: omitting `--description` or `--tag` keeps the saved values, `--description ""` clears the notes, and a single `--tag ""` clears the tags. Tags are trimmed and de-duplicated ignoring case, keeping the first spelling.

For host-key confirmation, launch the same alias with the values from the `HOSTKEY_UNKNOWN` or `HOSTKEY_MISMATCH` failure:

```text
--codex-edit --alias <alias> --observed-fingerprint <observed_fingerprint> --observed-algorithm <observed_algorithm> --result-file <result.json>
```

The alias must already exist; otherwise the window shows `HOST_NOT_FOUND`, no prompt appears, and closing it yields `cancelled`. Accepting stores the new fingerprint and algorithm but leaves the host unverified until a later successful probe or test.

## Run the window as the interactive user

- Launch with `Start-Process -WindowStyle Normal -PassThru` as the interactive Windows user who owns the profiles and credentials. A sandbox process may start without a window on that user's desktop. When that happens, use the platform's approved interactive-user execution path; keep hidden launches for tool mode only.
- Verify that the window is visible with the available desktop or window inspection before saying it is open. A PID, a successful `Start-Process`, or a nonzero window handle does not prove visibility. If visibility cannot be inspected, report only that the launch was requested and keep the result pending.
- Keep the process handle and the unique result path, and wait in bounded intervals while the user enters values. A tool call that yields a session ID is still running.
- Before relaunching an apparently invisible window, inspect the original process and result file to avoid duplicate editors or losing a completed save. Do not terminate unrelated app instances or temporary-secret sessions.

## Read the result

The result file is written once, when the window closes:

```json
{"status":"saved","alias":"example"}
```

- `saved`: the profile and any entered credentials are stored; continue with the host work.
- `trusted`: the observed host key is now pinned; retry the failed operation once.
- `cancelled`: the user declined, or the window closed without saving; end that flow and report it.
- No file: the window has not closed, or the launch failed. A missing result is never success.

Delete only the result file you created.
