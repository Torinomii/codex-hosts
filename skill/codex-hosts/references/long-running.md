# Long-running work and progress checks

Read this before starting anything that may take more than a few minutes or must not be repeated (builds, migrations, backups, test suites). codex-hosts has no job tools by design: the host's own multiplexer keeps the work alive, and every command below is an ordinary `exec` you write.

For work that must survive a lost client, start it under a host-side job manager and record a job ID or completion log. After a disconnect, cancellation or timeout, a command already started may continue or may have exited. Check the existing job and its recorded exit status before deciding whether a restart is needed.

## Detach

Detect once: `command -v tmux || command -v screen || echo none`. Use tmux, else screen, else `nohup`. If the user wants durable jobs and neither multiplexer exists, say so and suggest installing tmux; do not install it yourself.

```sh
# tmux
tmux new-session -d -s build 'make -j4 > build.log 2>&1; echo EXIT=$? >> build.log; tmux wait-for -S build-done'
tmux has-session -t build 2>/dev/null && echo running || echo session-absent
tmux wait-for build-done            # blocks until the job signals completion
tmux kill-session -t build          # stop it

# screen
screen -dmS build sh -c 'make -j4 > build.log 2>&1; echo EXIT=$? >> build.log'
screen -ls | grep -q build && echo running || echo session-absent

# nohup
nohup sh -c 'make -j4 > build.log 2>&1; echo EXIT=$? >> build.log' > /dev/null 2>&1 & echo $! > build.pid
kill -0 "$(cat build.pid)" 2>/dev/null && echo running || echo pid-absent

tail -n 40 build.log                # progress, for any of them
```

On a Windows host, use its existing job runner when the work must survive SSH disconnection. Record its job identifier and use `Get-Content -Tail 40 build.log` to inspect progress when the job writes that log.

## Check without polling

- Bundle status and log checks into one `exec_many` so one authentication serves them, for example `["tmux has-session -t build && echo running || echo session-absent", "tail -n 40 build.log"]`.
- A blocking check such as `tmux wait-for build-done` returns when the job signals. Set `command_timeout_ms` to the acceptable wait in milliseconds and allow enough time for the whole call in `tool_timeout_sec` (seconds). If the wait times out, check the existing job and log before waiting again.
- Leave at least a few minutes between checks, whether or not the host keeps its session; on a per-call host each check is a new authentication and hardware touch.
- `exec` returns only when its command ends; nothing streams. Make each check print exactly what you need next.
