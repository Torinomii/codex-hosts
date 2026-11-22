# Installation and version checks

Install the whole Skill directory as a symbolic link. Use PowerShell 7.2 or later.

| Starting point | Command |
|---|---|
| Source checkout | `pwsh -NoProfile -File .\scripts\install-local-skill.ps1` |
| Extracted release ZIP | `pwsh -NoProfile -File .\install-local-skill.ps1 -ReleasePackage` or double-click `install.cmd` |

The source installer checks Rust, MSVC and the Windows SDK and asks before compiling and installing links. The release already contains the executable. Let the user confirm replacements when the installer shows old and new targets. It retains backups. Use `-CodexHome` for another client's Skill home. Keep the source or extracted directory in place and rerun the installer if it moves. [Installer options and upgrades](https://github.com/Torinomii/codex-hosts#installation)

Register the linked executable in the client's MCP configuration. For Codex, use `~/.codex/config.toml` or the workspace's `.codex/config.toml` and substitute the actual installed path.

```toml
[mcp_servers.codex-hosts]
command = "C:\\Users\\<user>\\.codex\\skills\\codex-hosts\\bin\\codex-hosts.exe"
args = ["--mcp"]
startup_timeout_sec = 20
tool_timeout_sec = 86400
```

The client timeout is in seconds and includes the user's review time. Remote tool timeouts are in milliseconds. For a client approval card on each probe, set `approval_mode = "prompt"` for `probe` and `batch_probe`.

Click the version in the lower-right corner of the app to compare **Installed app** with **Installed Skill**, read from the installed [VERSION](../VERSION) file. **Bundled Skill** appears if that file is missing, invalid or differs from the embedded version. From the Skill directory, `bin\codex-hosts.exe --version | more` prints the windowed program's version.

Use the running server's `initialize` instructions and tool schemas to establish available features. If they differ from the Skill, report the mismatch and arrange an authorized app update or restart. Editing linked documentation takes effect on the next Skill read. A client that cached its instructions may need a new session.
