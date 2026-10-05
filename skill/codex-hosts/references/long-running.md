# Long-running work and progress checks

Read this before starting anything that may take more than a few minutes or must not be repeated (builds, migrations, backups, test suites). codex-hosts has no job tools by design: the host's own multiplexer keeps the work alive, and every command below is an ordinary `exec` you write.

Never hold long work inside one `exec`: a dropped link, a Codex restart, or a closed session kills it and it must start over. Detach it on the host, then check on it.

## Detach

Detect once: `command -v tmux || command -v screen || echo none`. Use tmux, else screen, else `nohup`. If the user wants durable jobs and neither multiplexer exists, say so and suggest installing tmux; do not install it yourself.

```sh
# tmux
tmux new-session -d -s build 'make -j4 > build.log 2>&1; echo EXIT=$? >> build.log; tmux wait-for -S build-done'
tmux has-session -t build 2>/dev/null && echo running || echo finished
tmux wait-for build-done            # blocks until the job signals completion
tmux kill-session -t build          # stop it

# screen
screen -dmS build sh -c 'make -j4 > build.log 2>&1; echo EXIT=$? >> build.log'
screen -ls | grep -q build && echo running || echo finished

# nohup
nohup sh -c 'make -j4 > build.log 2>&1; echo EXIT=$? >> build.log' > /dev/null 2>&1 & echo $! > build.pid
kill -0 "$(cat build.pid)" 2>/dev/null && echo running || echo finished

tail -n 40 build.log                # progress, for any of them
```

On a host confirmed to be Windows with PowerShell (`remote_env` or earlier output), `Start-Process -NoNewWindow -RedirectStandardOutput build.log` detaches and `Get-Content -Tail 40 build.log` reads.

## Check without polling

- Bundle checks into one `exec_many` so one authentication serves them, for example `["tmux has-session -t build && echo running || echo finished", "tail -n 40 build.log", "nvidia-smi --query-gpu=utilization.gpu --format=csv"]`.
- Prefer one blocking wait to repeated polling: an `exec` whose command returns when something happens (`tmux wait-for build-done`, or `timeout 540 tail -f --pid="$(cat build.pid)" build.log`), with `command_timeout_ms` set to the longest acceptable wait and below `tool_timeout_sec`. On return its output is what is new; on timeout, check once and wait again.
- Leave at least a few minutes between checks, whether or not the host keeps its session; on a per-call host each check is a new authentication and hardware touch.
- `exec` returns only when its command ends; nothing streams. Make each check print exactly what you need next.
