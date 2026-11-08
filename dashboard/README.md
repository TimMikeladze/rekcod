# rekcod dashboard

The web front end for rekcod: what runs on this Mac and on the fleet, with the menu bar's
actions. How it works and its limits: [`../docs/dashboard.md`](../docs/dashboard.md).

```sh
npm install
npm run dev        # http://localhost:4500, or the next free port; 127.0.0.1 only
npm start          # after `npm run build`
```

Both print a `/auth?token=...` link with a new token each run. Open that one: it keeps the
token in that origin's `localStorage`, and the page sends it to every API route as
`Authorization: Bearer`. Without it a page shows "Open this with `rekcod dashboard`" and the
API answers 401. There's no cookie: cookies ignore ports, so every other localhost server
would get it. `REKCOD_DASHBOARD_TOKEN` (32+ characters) keeps the same token across restarts.

Client code calls the API through `api()` in `src/lib/api.ts`, which adds the header; lint
fails on a bare `fetch` or `EventSource` elsewhere in `src/`. Streams are read with
`readEvents()` (`src/lib/sse.ts`), images with `api()` and a blob URL.

`REKCOD_BIN=/path/to/rekcod` if the right binary isn't on `PATH` (it needs `rekcod state`,
which older builds don't have).

Without a fleet, serve the fixture. Actions become dry runs:

```sh
REKCOD_STATE_FILE=$PWD/fixtures/state.json npm run dev
```

## Checks

```sh
npm test           # allow-list, issue ranking, docker stats parsing, the token and request guard, stream backpressure
npm run typecheck
npm run lint
npm run test:e2e   # Playwright against fixtures/state.json on port 4599 (E2E_PORT to change it)
```
