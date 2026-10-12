# Default socket: clients that ignore docker contexts

`rekcod start` switches the docker *context*, which only the docker CLI (and tools that
read `~/.docker/config.json`) honour. Plenty of clients don't: oxker, lazydocker builds,
SDKs, IDE plugins. They dial `/var/run/docker.sock`, which on most Macs is a root-owned
symlink to OrbStack's or Docker Desktop's socket, so they say "cannot connect to the
docker daemon" while rekcod runs.

## Approach

A two-level symlink, so root is needed exactly once:

```
/var/run/docker.sock  (root, set once)  ->  ~/.rekcod/default.sock  (user-owned)  ->  active engine socket
```

- `rekcod socket install` (sudo, once): records what `/var/run/docker.sock` pointed at,
  points `~/.rekcod/default.sock` there, then `sudo ln -sfn ~/.rekcod/default.sock /var/run/docker.sock`.
  If rekcod is running, `default.sock` points at rekcod straight away.
- `rekcod start` (unless `--no-switch`): retargets `default.sock` at `~/.rekcod/run/docker.sock`,
  remembering the previous target in `~/.rekcod/run/prev-default-sock`. No sudo; atomic rename.
- `rekcod stop`: points `default.sock` back at the previous target.
- `rekcod socket uninstall` (sudo): puts `/var/run/docker.sock` back to the recorded original
  and removes `default.sock`.
- First `rekcod start` from a terminal runs `socket install` itself (one sudo prompt), recorded in
  `~/.rekcod/default.sock.asked` so it never asks again; declining just prints a tip from then on.
  Without a terminal (menu bar) it prints the hint and the menu offers the item.
- Not installed afterwards: `rekcod start` prints a one-line hint. `rekcod status` shows the state.
- Another engine (OrbStack) rewriting `/var/run/docker.sock` on its own start just makes the
  link "not installed" again; nothing breaks.

Generic: works for every client that uses the default socket, no env vars, no per-tool config.
`DOCKER_HOST` still overrides everything (start keeps warning about it).

## Pieces

- `crates/rekcod/src/syssock.rs`: state (`Linked`/`Other`/`Missing`), install/uninstall,
  retarget on start/stop. `REKCOD_SYSTEM_SOCKET` overrides `/var/run/docker.sock` for tests.
- CLI `rekcod socket install|uninstall`; menu bar `Kind::SocketInstall` (interactive: sudo
  needs a terminal), offered while not installed.
- Tests: retarget/restore round-trip on temp paths; parity tests cover the new kind.
