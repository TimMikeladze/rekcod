# Dashboard: round 4

Twenty changes to the dashboard (`dashboard/`). None adds a `Kind`: every new button runs an
action `rekcod state` already offers, through the same `trigger` (confirm, allow-list). The
only new route is read-only and runs no command.

## The list

1. **Sortable columns.** Containers (name, CPU, memory, status), images (size, created),
   volumes (size), apps (name, req/min, 5xx), jobs (started, took). The column and direction
   live in the URL (`?sort=mem`, `?sort=-size`); a header click cycles asc, desc, off.
2. **Bulk select.** A checkbox column on containers, images and volumes, `x` toggles the
   focused row. A bar at the bottom offers the action kinds every selected row has (stop,
   restart, remove). One confirm lists each `rekcod` line; then each item's own action runs
   through `/api/run`, one after another.
3. **Container tabs.** `/containers/NAME?tab=overview|logs|inspect|env`, a segmented row like
   dux's `TabRow`. Inspect shows the raw JSON.
4. **Log viewer.** Lines get numbers and warn/error colours; search takes a regex (`.*`
   toggle), counts matches, Enter/Shift+Enter jump between them; "Next error" jumps to the
   next error line; "Only matching" hides the rest; "Copy" copies the lines shown.
5. **Overview sparklines.** The CPU and Memory tiles draw the chart window's total.
6. **App traffic columns.** `/apps` gets a req/min sparkline (last hour) and a 5xx-rate column.
7. **Jobs timeline.** A 24 h lane chart above the jobs table: one lane per job name, a bar
   from start to finish (or now), coloured by result.
8. **Fleet tabs.** `/fleet?view=nodes|namespaces|edge|bans|remotes`, the same segmented row.
9. **Shortcut sheet.** `?` opens a dialog listing every key; `g` then a letter goes to a page
   (`g c` containers, `g a` apps, ...). The letters live in `nav.ts`.
10. **Palette upgrades.** "Pages" entries (every nav item), rows go straight to their detail
    page, recently picked entries first (localStorage), and a confirm action's warning shown
    under it.
11. **Getting-started empty states.** No remote, no apps, no jobs, no containers, no
    sandboxes: what to run, with copy buttons (`CopyCommand`).
12. **Pending rows.** `pending` becomes a set of argv keys; a row whose action runs shows a
    spinner and disables its buttons. The success toast has "View", which opens
    `/activity?run=ID` (the run route already returns the id).
13. **Change highlight.** `NavRow` takes `watch`; when it changes the row flashes for 1.5 s.
    `prefers-reduced-motion` gets a static tint instead of the fade.
14. **Activity filters and "Run again".** Failed only, a subject picker, text search, all in
    the URL. "Run again" shows when the same argv is still offered (`findOffered` on the
    client's state) and goes through `trigger`.
15. **Disk breakdown.** One stacked bar (images, containers, volumes, build cache), the five
    largest images and volumes linking to their lists, and what a prune would free.
16. **Node history.** The hub keeps load, memory and disk per node from each full state for
    an hour; `GET /api/node-history?id=` (an id in the current state) returns it, and
    `/nodes/ID` charts it. Fixture mode generates an hour.
17. **Pins.** A pin button on container and app pages; pinned items sit under Overview in the
    sidebar with their state dot (localStorage `rekcod.pins`).
18. **Density.** Comfortable or compact tables, a switch next to the theme. `<html
    data-density>`, set before first paint like the sidebar fold.
19. **Icon and title.** `app/icon.svg` from the rekcod mark (dark-aware), `app/apple-icon.png`, and the tab title
    starts with the problem count: `(2) Apps · rekcod`.
20. **Performance.** New states are structurally shared with the last one (`share()`), so
    unchanged rows keep their object and memoised rows skip rendering. Logs render only the
    lines near the view; wrapped lines are measured as they render.

## Pieces

- `lib/share.ts`: `share(prev, next)`, deep-equal parts keep `prev`'s reference.
- `hooks/use-sort.ts`, `hooks/use-local.ts` (localStorage with try/catch), `hooks/use-flash.ts`.
- `components/shell/tab-row.tsx`, `components/shell/shortcuts.tsx`, `components/shell/pins.tsx`.
- `components/bulk.tsx`: selection context and the bar.
- `components/get-started.tsx`.
- `lib/server/nodes.ts` + `app/api/node-history/route.ts`.
- Charts stay in `resource-chart.tsx` (overview sparkline) and `fleet-charts.tsx` (app
  sparkline, jobs timeline, node history).

## Tests

Unit: `share`, sort comparators, log line classification and regex search, palette pages and
recents, node history route validation. E2E: one happy path per item and the edge case that
matters (bulk confirm cancel runs nothing, a bad regex, Run again absent for an argv no longer
offered).
