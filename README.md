# mesh

A tiny native binary that joins your devices into an **opencode mesh**: from any opencode session, on any device, the agent can run anything (including the `opencode` CLI) on any other device.

On my machines, running a command on another device takes about 35 ms, and mesh idles at 0.8 MB of RAM.

```sh
# install + join, on each device (needs gh, opencode, Tailscale)
gh api repos/Hona/mesh/contents/install.sh -H "Accept: application/vnd.github.raw" | sh
gh api repos/Hona/mesh/contents/install.ps1 -H "Accept: application/vnd.github.raw" | Out-String | iex  # Windows
```

```sh
mesh exec all opencode upgrade && mesh exec all opencode service restart
mesh exec server opencode run "pull main and run the tests"
mesh exec laptop -c 'docker ps | grep web'
mesh cp -r ./dist server:~/deploy/
mesh exec -t server opencode     # server's TUI, in this terminal
mesh status
```

Or just ask your agent: *"run the tests on the server"*.

Design: [SPEC.md](SPEC.md)
