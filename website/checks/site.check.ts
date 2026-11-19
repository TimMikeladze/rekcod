import { describe, expect, test } from 'bun:test';
import { existsSync, readFileSync, statSync } from 'node:fs';
import path from 'node:path';
import { links, load, themeKey, url } from '../src/lib/landing';

// Checks the built site in dist/. Run after `bun run build`: `bun run check:site`. Named
// .check.ts so a bare `bun test`, which runs before the build in CI, doesn't pick it up.

const dist = path.resolve(import.meta.dir, '../dist');
const read = (p: string) => readFileSync(path.join(dist, p), 'utf8');
const { landing } = load();
const pages = { landing: read('index.html'), docs: read('docs/index.html') };

describe('built pages', () => {
  test('are self-contained: one inline stylesheet, no remote assets, only the allowed scripts', () => {
    for (const html of Object.values(pages)) {
      expect(html).not.toMatch(/<link[^>]+rel="stylesheet"/);
      expect(html).not.toMatch(/(?:src|href)="https?:\/\/[^"]+\.(?:css|js|woff2?)"/);
      expect(html).not.toMatch(/src="http/);
      const scripts = [...html.matchAll(/<script([^>]*)>([\s\S]*?)<\/script>/g)];
      for (const [, attrs, body] of scripts) {
        if (attrs.includes('application/ld+json')) continue;
        // Executable scripts are same-origin files: vercel.json's CSP is script-src 'self'.
        expect(attrs).toMatch(/src="\//);
        expect(body.trim()).toBe('');
      }
      expect(html.indexOf('src="/theme.js"')).toBeLessThan(html.indexOf('<style'));
    }
    expect(read('theme.js').split('\n').length).toBeLessThan(30);
    expect(read('theme.js')).toContain(`'${themeKey}'`);
    expect(pages.landing).not.toMatch(/<script type="module"/);
  });

  test('ship both themes, the OS fallback and a three-state toggle in the header', () => {
    const html = pages.landing;
    // The build minifies the CSS: attribute values lose their quotes.
    expect(html.includes(':root[data-theme=light]{')).toBe(true);
    expect(/@media \(prefers-color-scheme: ?light\)\{:root:not\(\[data-theme=dark\]\):not\(\[data-theme=light\]\)/.test(html)).toBe(true);
    expect(html).not.toMatch(/<html[^>]*data-theme=/);
    const header = html.slice(html.indexOf('<header'), html.indexOf('</header>'));
    expect(header).toContain('data-theme-toggle');
    for (const s of ['t-system', 't-dark', 't-light']) expect(header).toContain(s);
  });

  test('body and soft text pass 4.5:1 on paper in both themes', () => {
    const css = pages.landing;
    const block = (selector: string) => css.slice(css.indexOf(selector), css.indexOf('}', css.indexOf(selector)));
    const lightness = (scope: string, name: string) => Number(scope.match(new RegExp(`${name}:\\s*oklch\\(([\\d.]+)%`))?.[1] ?? NaN) / 100;
    // Neutral OKLCH: relative luminance is L cubed.
    const contrast = (a: number, b: number) => (Math.max(a, b) ** 3 + 0.05) / (Math.min(a, b) ** 3 + 0.05);
    for (const scope of [block(':root{color-scheme:dark'), block(':root[data-theme=light]{')]) {
      const paper = lightness(scope, '--paper');
      expect(contrast(lightness(scope, '--body'), paper)).toBeGreaterThanOrEqual(4.5);
      expect(contrast(lightness(scope, '--soft'), paper)).toBeGreaterThanOrEqual(4.5);
    }
  });

  test('carry the head metadata', () => {
    const html = pages.landing;
    const meta = (attr: string) => html.match(new RegExp(`<meta (?:name|property)="${attr}" content="([^"]*)"`))?.[1];
    expect(html).toContain(`<title>${landing.title}</title>`);
    expect(meta('description')!.length).toBeLessThanOrEqual(160);
    expect(html).toContain(`<link rel="canonical" href="${url('/')}">`);
    expect(pages.docs).toContain(`<link rel="canonical" href="${url('/docs/')}">`);
    expect(html).not.toContain('index.html"');
    for (const p of ['og:title', 'og:description', 'og:type', 'og:site_name', 'og:image:alt']) expect(meta(p)).toBeTruthy();
    expect(meta('og:url')).toBe(url('/'));
    expect(meta('og:image')).toBe(url('/og.png'));
    expect(meta('og:image:width')).toBe('1200');
    expect(meta('og:image:height')).toBe('630');
    expect(meta('og:image:type')).toBe('image/png');
    expect(meta('twitter:card')).toBe('summary_large_image');
    expect(meta('twitter:image')).toBe(url('/og.png'));
    const ld = JSON.parse(html.match(/<script type="application\/ld\+json">([\s\S]*?)<\/script>/)![1]);
    expect(ld.name).toBe(landing.name);
    expect(ld.softwareVersion).toBe(landing.version);
    expect((html.match(/<h1/g) ?? []).length).toBe(1);
    expect(html).not.toContain('\u2014');
  });

  test('every capability section is a labelled section with an explanation holding code', () => {
    for (const c of landing.capabilities) {
      const at = pages.landing.indexOf(`id="${c.id}"`);
      expect(at).toBeGreaterThan(0);
      const section = pages.landing.slice(at, pages.landing.indexOf('</section>', at));
      expect(section).toContain(`aria-labelledby="${c.id}-title"`);
      expect(section).toMatch(/<p class="explain">[\s\S]*?<code>/);
    }
  });

  test('every README section reaches the docs page', () => {
    for (const slug of load().readme.slugs) expect(pages.docs).toContain(`id="${slug}"`);
  });

  test('diagram labels expose keyboard-focusable tooltips with descriptions', () => {
    const diagram = pages.landing.match(/<svg class="arch"[\s\S]*?<\/svg>/)?.[0] ?? '';
    const triggers = [...diagram.matchAll(/<g[^>]+data-arch-trigger[^>]*>/g)].map(([tag]) => tag);
    expect(diagram).toContain('viewBox="0 0 560 616"');
    expect(triggers).toHaveLength(13);
    for (const trigger of triggers) {
      expect(trigger).toContain('role="button"');
      expect(trigger).toContain('tabindex="0"');
      const id = trigger.match(/aria-describedby="([^"]+)"/)?.[1];
      expect(id).toBeTruthy();
      expect(pages.landing).toMatch(new RegExp(`<div id="${id}"[^>]+role="tooltip" hidden[^>]*>[\\s\\S]*?<span>[^<]+</span>`));
    }
  });

  test('visible workflow notes follow the drawing', () => {
    const html = pages.landing;
    const start = html.indexOf('class="hero-workflow shell"');
    expect(start).toBeGreaterThan(html.indexOf('</figure>'));
    const notes = html.slice(start, html.indexOf('</section>', start)).replace(/<[^>]*>/g, ' ');
    const labels = ['rekcod start', 'Docker Desktop', 'docker compose up -d', 'upstream Docker Engine', 'Bind mounts', 'Virtualization framework', 'linux/arm64', 'linux/amd64', 'images use Rosetta'];
    let previous = -1;
    for (const label of labels) {
      const at = notes.indexOf(label);
      expect(at).toBeGreaterThan(previous);
      previous = at;
    }
  });

  test('agent files exist and the sitemap names only built files', () => {
    for (const f of ['llms.txt', 'AGENTS.md', 'index.md', 'robots.txt', 'sitemap.xml', 'og.png', 'theme.js', 'site.js']) expect(existsSync(path.join(dist, f))).toBe(true);
    expect(read('robots.txt')).toContain(`Sitemap: ${url('/sitemap.xml')}`);
    for (const [, loc] of read('sitemap.xml').matchAll(/<loc>([^<]+)<\/loc>/g)) {
      const file = new URL(loc).pathname.replace(/\/$/, '/index.html').slice(1);
      expect(existsSync(path.join(dist, file))).toBe(true);
    }
  });

  test('og.png is a 1200x630 PNG under 300 KB, drawn from the page\'s H1', () => {
    expect(readFileSync(path.resolve(import.meta.dir, '../scripts/og-card.html'), 'utf8')).toContain(`<h1>${landing.h1}</h1>`);
    const png = readFileSync(path.join(dist, 'og.png'));
    expect(png.subarray(1, 4).toString()).toBe('PNG');
    expect(png.readUInt32BE(16)).toBe(1200);
    expect(png.readUInt32BE(20)).toBe(630);
    expect(statSync(path.join(dist, 'og.png')).size).toBeLessThan(300_000);
  });

  test('header and footer carry the project links in order', () => {
    const html = pages.landing;
    const header = html.slice(html.indexOf('<header'), html.indexOf('</header>'));
    const footer = html.slice(html.indexOf('<footer'), html.indexOf('</footer>'));
    const order = (part: string, labels: string[]) => labels.map((l) => part.indexOf(`<span class="sr-only">${l}</span>`));
    const named = (icon: string) => links.find((l) => l.icon === icon)!.label;
    const headerAt = order(header.slice(header.indexOf('icon-links')), ['github', 'x', 'linkedin'].map(named));
    expect(headerAt.every((i) => i > 0)).toBe(true);
    expect([...headerAt].sort((a, b) => a - b)).toEqual(headerAt);
    const footerIcons = footer.slice(footer.indexOf('footer-icons'));
    const footerAt = order(footerIcons, ['github', 'x', 'linkedin', 'discord'].map(named));
    expect(footerAt.every((i) => i > 0)).toBe(true);
    expect([...footerAt].sort((a, b) => a - b)).toEqual(footerAt);
    expect(header).toContain('https://www.linkedin.com/in/tim-mikeladze');
    expect(header).toContain('https://x.com/linesofcode');
    expect(footer).toContain('https://discord.com/users/linesofcode');
  });

  test('the hero offers a copyable quickstart before the architecture and feature examples', () => {
    const html = pages.landing;
    const start = html.indexOf('data-panel="humans"');
    const agentPanel = html.indexOf('data-panel="agents"');
    const diagram = html.indexOf('class="arch"');
    expect(start).toBeGreaterThan(0);
    expect(agentPanel).toBeGreaterThan(start);
    expect(diagram).toBeGreaterThan(agentPanel);
    expect(diagram).toBeLessThan(html.indexOf(`id="${landing.capabilities[0].id}"`));
    const quickstart = html.slice(start, agentPanel);
    expect(quickstart).toContain('aria-label="Copy install and run commands"');
    const copied = quickstart.match(/data-copy="([^"]*)"/)?.[1];
    expect(copied?.replace(/&#10;/g, '\n')).toBe(landing.start.example.block.value.replace(/&/g, '&amp;'));
    const agents = html.slice(agentPanel, diagram);
    for (const [, href] of agents.matchAll(/href="([^"]+)"/g)) expect(existsSync(path.join(dist, href))).toBe(true);
  });
});
