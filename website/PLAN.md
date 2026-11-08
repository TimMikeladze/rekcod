# Markdown website

Build a minimal landing page and documentation reader, then deploy to Vercel.

## Source and design

`../README.md` supplies the complete landing page, including its introduction, install
command, examples and measurements. `../docs/README.md` supplies the docs index and
sidebar groups. Render repository Markdown at build time; do not maintain separate
website copy or fetch GitHub from the browser. Resolve Markdown links to generated
pages, anchors to GitHub-compatible IDs, and source-file links to GitHub.

Use Astro static output with Bun and TypeScript. This keeps the pages readable without
JavaScript and avoids introducing a backend. Use a narrow reading column, white
background, black text, self-hosted Geist, small navigation, and quiet separators.
Docs have a grouped sidebar, heading navigation and a mobile document selector.
Code blocks support copying. Search filters the generated document titles and text.
The user subsequently specified an upside-down Docker logo, black and white. Use
`public/logo.svg` in the header and favicon, with a 180-degree rotation baked into the SVG.

## Implementation

- [x] Compile Markdown, metadata, navigation and links; test content fidelity, link
  resolution, duplicate headings, unsafe HTML and newly added Markdown.
- [x] Build the landing page and docs routes with responsive shared styles, search,
  copy controls, source links, metadata, a sitemap and a 404 page.
- [x] Run tests, Astro type checks and the production build. Verify navigation,
  search, copying and desktop/mobile layout in a browser.
- [x] Deploy the repository to Vercel, connect GitHub for Markdown updates, and verify
  the public landing page and a deep documentation URL.

## Review focus

Relative links need the source file's directory, not the current website URL.
Fragment IDs must match GitHub, including inline code and repeated headings.
New docs must get routes without a source-code edit. Script HTML must not execute.
Wide tables and code must scroll inside the reading column on phones.

## Deployment

Build from the repository root so the parent Markdown is present. Vercel runs
`cd website && bunx bun@1.4.4 run build` and publishes `website/dist`. Connect the existing
public GitHub repository; keep its visibility unchanged. Use the existing
`linesofcode` Vercel team. No domain was requested; use the assigned Vercel domain.

## Verification

Five Bun tests pass; Astro reports no errors, warnings or hints. The static build
produces 59 pages and the link check passes all 4,182 local links. Playwright verified
search (including no results), copying the exact code text, navigation, mobile
selection and contained table scrolling. Six axe checks across the landing page,
getting-started guide and benchmark guide at 1440px and 390px report no violations.
A fresh reviewer returned `ship` with no material findings.

Production is https://rekcod.vercel.app. Public checks verified the landing page,
docs index, a deep guide, website guide, search JSON, sitemap and a 404. Production
browser checks verified search, clipboard text, mobile navigation and no page errors.

Vercel's bundled Bun 1.3.14 could not parse lockfile v2. The install and build commands
now explicitly run Bun 1.4.4 through bunx; the second production build reached READY.
