# rekcod dashboard

The web front end for rekcod: what runs on this Mac and on the fleet, with the menu bar's
actions. How it works and its limits: [`../docs/dashboard.md`](../docs/dashboard.md).

```sh
npm install
npm run dev        # http://localhost:4500, or the next free port; 127.0.0.1 only
npm start          # after `npm run build`
```

`REKCOD_BIN=/path/to/rekcod` if the right binary isn't on `PATH` (it needs `rekcod state`,
which older builds don't have).

Without a fleet, serve the fixture. Actions become dry runs:

```sh
REKCOD_STATE_FILE=$PWD/fixtures/state.json npm run dev
```

## Checks

```sh
npm test           # allow-list, issue ranking, docker stats parsing
npm run typecheck
npm run lint
```
