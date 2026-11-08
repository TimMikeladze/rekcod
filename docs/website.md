# Website

The landing page renders the root [README](../README.md). The docs viewer renders
Markdown in `docs/`, the root contributing, changelog and security files, the examples,
and the guest filesystem README. The sidebar groups and order come from
[docs/README.md](README.md). New Markdown files in `docs/` get a page automatically;
link them from the index to choose where they appear in navigation.

## Run locally

```sh
cd website
bun install --frozen-lockfile
bun run dev
```

Astro prints the local URL and chooses another port if its default is occupied.
The site is a separate app from `dashboard/`. Run website commands from `website/`
so the build can find the repository Markdown in its parent directory.

## Check the site

```sh
cd website
bun test
bun run typecheck
bun run build
bun run check:links
```

The link check verifies local pages and heading fragments in the built HTML.
Markdown links to source files and GitHub line numbers stay on GitHub. Code blocks,
tables and heading IDs use GitHub conventions. Raw HTML is sanitized before rendering.

## Deploy

Vercel builds from the repository root using `vercel.json`, installs with Bun and
publishes `website/dist`. The Vercel project is `rekcod` under `linesofcode` and is
connected to this GitHub repository. Pushes to `main` rebuild the site, including
changes that only touch Markdown.

The deployment commands use `bunx bun@1.4.4` so Vercel's bundled Bun version does not
need to understand the newer lockfile format.

The assigned production domain is configured in `website/astro.config.mjs`.
Set `SITE_URL` in the Vercel build environment if the domain changes; it controls
canonical URLs and the sitemap.

The browser loads rendered HTML. It does not call GitHub to retrieve content.
Search loads a static index on its first use. The pages and navigation work without
JavaScript; search, code copying and the mobile document selector use JavaScript.
