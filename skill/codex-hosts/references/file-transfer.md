# Upload a small file over SSH

Use `exec_stdin` to send a small local file to the requested SSH host. Its `stdin` is UTF-8 text with a 1 MiB limit. The entire encoded payload goes in the call, so keep it to a few tens of KB of base64 in practice. Telnet has no stdin tool.

1. Encode the file locally as base64, including text files to preserve their bytes. For a directory, create an archive first.
2. Create the destination directory. Pass the base64 as `stdin` and select a decoder for the remote shell.

   | Remote shell | Example command |
   |---|---|
   | POSIX | `base64 -d > /path/file` |
   | Windows cmd or PowerShell | `powershell -NoProfile -NonInteractive -Command "[IO.File]::WriteAllBytes('C:\path\file.zip',[Convert]::FromBase64String([Console]::In.ReadToEnd()))"` |

3. Compare the remote file's hash (`sha256sum` or `Get-FileHash`) with the local hash. For an archive, verify it before extracting it with the host's archive tool.

For larger transfers, use `exec` to fetch from a repository, release or package source the host already reaches, or ask the user to copy the file. Avoid splitting large payloads across calls or setting up another transport.
