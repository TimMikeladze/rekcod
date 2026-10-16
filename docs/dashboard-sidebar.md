# Dashboard: sidebar shell

The dashboard moves from one front page with a tab strip to the shell the dux console uses
(`../dux/console/src/components/shell`): a fixed sidebar, a 56 px bar on top of every page and
one centred content column. The look is dux's too: black and white, colour only for state.

## What changes

- Each tab becomes a page: `/containers`, `/images`, `/volumes`, `/apps`, `/jobs`,
  `/sandboxes`, `/projects`, `/ports`, `/fleet`. The list pages and the detail pages under
  them (`/containers/NAME`, `/apps/NAME`) share a URL prefix, so the sidebar marks the
  section you're in. `/?tab=X&q=Y` still works: it redirects to `/X?q=Y`.
- `/` keeps the tiles, "Needs attention" and the charts. Nothing else.
- Sidebar, top to bottom: the rekcod mark with the VM's state (a menu with the VM and
  storage actions), Find (Cmd+K), Overview, then groups "This Mac" (Containers, Images,
  Volumes, Sandboxes, Projects, Ports), "Fleet" (Apps, Jobs, Fleet) and "System" (Disk, VM,
  Activity). Counts sit right of each item. At the bottom: the remote and namespace menu,
  the docker host menu, and the theme switch (System, Light, Dark).
- The top bar shows where you are (section, then the item) and on the right: live/refresh,
  Activity, the bell.
- Below `md` the sidebar is a drawer behind a menu button in the top bar (one bar on a phone, not two).
- Colours come from dux's tokens (`globals.css`): `--ok`, `--warn`, `--alert`, `--success`,
  `--hairline`, `--subtle`. Status dots and meters use them instead of Tailwind's emerald/
  amber/red.
- Theme moves to `next-themes` (already a dependency), so the pick is remembered.

## Pieces

- `components/shell/`: `sidebar.tsx` (frame, drawer, nav, menus), `top-bar.tsx`,
  `page-title.tsx`, `theme-switch.tsx`, `nav.ts` (sections, their paths, counts).
- `components/dashboard.tsx`: `Shell` lays out sidebar + bar + column; `Overview` is the
  front page; `SectionPage` renders one list with its filter.
- `lib/issues.ts` keeps its `tab`; "Needs attention" links to `/<tab>`.
- Keyboard stays: `/` filter, 1-9 jump to the nine list pages, Cmd+K. Cmd+B folds the sidebar
  into an icon rail, like shadcn's sidebar (`rail:` variant in `globals.css`, keyed on
  `<html data-sidebar="collapsed">`).

## Tests

The e2e suite visits `/<section>` instead of `/?tab=<section>` and checks the sidebar marks it
(`aria-current="page"`); one test covers the `/?tab=` redirect.
