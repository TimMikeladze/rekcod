# Website

The site at rekcod.vercel.app is an Astro static build in `website/`. README.md is its only
source: the landing page quotes it, the reference page renders all of it.

## Pages and files

| Path | What it is |
| --- | --- |
| `/` | Landing page: hero with an architecture drawing, split demo, one section per capability, each showing a README block |
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
- `diagram("Local Docker architecture")`: node and edge labels of that Mermaid block, which the
  hero drawing (`components/HeroDiagram.astro`) uses. The Apple Silicon strip under it is copy.

A reference that matches no block, or more than one, throws and fails the build. Captured
output in the README comes from real runs; the benchmark block names the Mac it ran on.

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

## Left out

No adoption figures (the repository has no stars to show), no adjacent-tools cards (no
sibling packages) and no scaffold command (rekcod has none).
