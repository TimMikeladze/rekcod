# Website

The site at rekcod.vercel.app is an Astro static build in `website/`. The landing page
uses examples from README.md. The reference page renders the whole README.

## Pages and files

| Path | What it is |
| --- | --- |
| `/` | The argument for rekcod: install, why Docker on a Mac needs a VM, what crosses between macOS and Linux, why it's built this way, the commands it adds, switching, benchmarks and limitations |
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
- `diagram("Local Docker architecture")`: node and edge labels of the README's Mermaid
  flowchart, used in the architecture drawing and its tooltips.
- `sequence("Shared files and change notifications")`: participants and messages of the
  README's sequence diagram. The page shows the messages from the FSEvents one onward.
- `list("limitations")`: the list under a README section, shown as it is with its `#anchor`
  links pointed at `/docs/`.

## Landing page sections

The page answers, in this order: what it is and how to install it, why Docker on a Mac needs
a VM, how a command, a file and a port get into that VM, why rekcod is built the way it is,
what you run day to day, how to try it beside another engine, what was measured and what
doesn't work.

| Section | `landing.ts` key | Shows |
| --- | --- | --- |
| Hero | `h1`, `lede`, `start` | The install block and agent links |
| Docker on a Mac runs in a Linux VM | `why` | Copy only |
| What crosses between macOS and Linux | `how` | The architecture drawing beside the five paths (Docker API, files, published ports, CPU architecture, memory), two of them with their measured round trip |
| How a Mac edit reaches a file watcher | `files` | The four messages after an edit, from the README's sequence diagram |
| Why it's built this way | `decisions` | Four decisions, each linked to its README section |
| What you run | `commands`, `capabilities` | One row per README section: copy on the left, its block on the right |
| Try it beside Docker Desktop or OrbStack | `switching` | The `rekcod import` block |
| Benchmarks | `measured` | The captured benchmark run and the README's paragraph on what each line measures |
| Limitations | `limitations` | The README's Limitations list |

The page makes no claim about another engine's speed or behavior. The README has no captured
run of one, so there is nothing to reference.

The drawing's labels open tooltips on hover, keyboard focus or tap.

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
