# Long-running work and progress checks

Read this before starting anything on a host that may take more than a few minutes or must not be repeated (builds, migrations, backups, test suites). codex-hosts has no job tools on purpose: the remote host's own multiplexer keeps the work alive, and every command below is an ordinary `exec` written by you.

## Rule

Never hold a long command inside a single `exec`. A dropped link, a Codex restart, or a closed session would kill it and the work would have to start over. Detach it on the host, then check on it.

## Detect a multiplexer once

```sh
command -v tmux || command -v screen || echo none
```

Use tmux when present, screen otherwise, and `nohup` only when neither exists. If the user wants durable jobs and the host has neither, say so and suggest installing tmux; do not install anything yourself.

## tmux

```sh
tmux new-session -d -s build 'make -j4 > build.log 2>&1; echo EXIT=$? >> build.log; tmux wait-for -S build-done'
tmux has-session -t build 2>/dev/null && echo running || echo finished
tail -n 40 build.log
tmux wait-for build-done            # blocks until the job signals completion
tmux kill-session -t build          # stop it
```

## screen

```sh
screen -dmS build sh -c 'make -j4 > build.log 2>&1; echo EXIT=$? >> build.log'
screen -ls | grep -q build && echo running || echo finished
tail -n 40 build.log
```

## nohup fallback

```sh
nohup sh -c 'make -j4 > build.log 2>&1; echo EXIT=$? >> build.log' > /dev/null 2>&1 & echo $! > build.pid
kill -0 "$(cat build.pid)" 2>/dev/null && echo running || echo finished
tail -n 40 build.log
```

## Checking progress without polling

- Bundle the checks into one `exec_many` call so one authentication serves all of them, for example `["tmux has-session -t build && echo running || echo finished", "tail -n 40 build.log", "nvidia-smi --query-gpu=utilization.gpu --format=csv"]`.
- Prefer one blocking wait to repeated polling: run `exec` with `command_timeout_ms` set to the longest you are willing to wait and a command that returns when something happens, such as `tmux wait-for build-done` or `timeout 540 tail -f --pid="$(cat build.pid)" build.log`. When it returns, the output is exactly what is new; when it times out, check once and wait again.
- Keep at least a few minutes between checks. Each check on a per-call host is a new authentication (and a hardware touch); on a host whose profile keeps the session it is not, but the rule still applies.
- `exec` returns only when the command ends; there is no streaming. Design each check so its output is what you need to see next.

## Windows hosts

On a Windows host with PowerShell, `Start-Process -NoNewWindow -RedirectStandardOutput build.log` detaches a job and `Get-Content -Tail 40 build.log` reads it. Confirm the host is Windows (`remote_env` or earlier output) before using either.
