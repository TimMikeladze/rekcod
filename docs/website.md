# Website

The site at rekcod.vercel.app is an Astro static build in `website/`. The landing page
uses examples from README.md. The reference page renders the whole README.

## Pages and files

| Path | What it is |
| --- | --- |
| `/` | Install and run a first container, use an existing Compose project, and see examples for Linux volumes, snapshots, toolchains and sandboxes |
| `/docs/` | README.md in full, with a sticky contents column and the Mermaid diagrams |
| `/llms.txt`, `/AGENTS.md`, `/index.md` | The landing page's model as Markdown, for agents |
| `/sitemap.xml`, `/robots.txt` | Generated from the same model |
| `/og.png` | 1200x630 card, rendered from `website/scripts/og-card.html` and committed |
| `/theme.js`, `/site.js` | Theme boot (blocking, in `<head>`) and the toggle, copy buttons and old-anchor redirect |

The old `/docs/NAME/` pages redirect to `/docs/#section` (astro.config.mjs). Links to a
`/#section` the landing page doesn't have are sent to `/docs/#section` by `site.js`.

## Rule

Copy (headings, the 1 to 3 sentences per section) lives in `website/src/lib/landing.ts`.
Examples don't: every command, file, table and figure on the page is a reference into a
README fenced block or table, resolved at build time by `website/src/lib/readme.ts`:

- `terminal("rekcod snap save seeded")`: the `console` block with that `$` line
- `snippet("dev.rekcod.local:")`: the block containing that line
- `table("Key")`: the table whose first header cell is that text
- figures: a regex over a captured block, never retyped
- `diagram("Local Docker architecture")`: technical labels from the README’s Mermaid block used
  in the hero workflow and its tooltips.

The hero’s SVG shows the Docker API and file-sharing paths from macOS into the Linux VM. Its
labels open tooltips on hover, keyboard focus or tap. The visible notes below show the two
commands to run an existing Compose project, what rekcod takes over from Docker Desktop and how
Apple Silicon runs ARM and Intel images.

A reference that matches no block, or more than one, throws and fails the build. Captured
output in the README comes from recorded runs. The benchmark section links to the commands and
records the Mac, software versions and VM size used to measure them.

## Pieces

- `readme.ts`: parses README.md into fenced blocks, tables and sections; the resolvers above.
- `landing.ts`: the page model. Section order, copy, references, links, version (the newest
  dated CHANGELOG.md release, which is what `install.sh` installs).
- `pages/index.astro`, `pages/docs/index.astro`, the endpoints: markup only.
- Design: near-black dark theme and a light theme, system fonts, OKLCH tokens in
  `styles/global.css`, one stylesheet inlined into each page. Theme preference is
  `localStorage["rekcod-theme"]` (system, dark, light). Scripts are files, not inline,
  because vercel.json's CSP is `script-src 'self'`.
- Tests: `bun test` checks references, figures, sections and the generated Markdown;
  `bun run check:site` checks the built `dist/` (self-contained pages, head metadata, files
  the sitemap names, og.png size, contrast in both themes, header and footer links).
