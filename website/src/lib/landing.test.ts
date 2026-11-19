import { describe, expect, test } from 'bun:test';
import { Readme, latestRelease } from './readme';
import { buildLanding, load, metaDescription, readRepo, url } from './landing';
import { agentsMd, llmsTxt, pageMarkdown } from './markdown';
import { consoleHtml } from './render';

const { readme, landing } = load();

describe('landing references into README.md', () => {
  test('every shown block is a README block, found exactly once', () => {
    const artifacts = [
      ...landing.capabilities.flatMap((c) => c.show),
      ...landing.showcase.tabs.map((t) => t.snippet),
      ...landing.guides.map((g) => g.snippet),
      ...landing.start.panels,
      landing.split.terminal,
      landing.measured.terminal,
    ];
    for (const a of artifacts) {
      if (a.kind === 'table') expect(readme.markdown).toContain(a.markdown);
      else expect(readme.markdown).toContain(a.block.value);
    }
    expect(readme.markdown).toContain(landing.install);
  });

  test('a reference to a missing or repeated example fails the build', () => {
    const edited = new Readme(readme.markdown.replace('$ rekcod snap save seeded', '$ rekcod snap save other'));
    expect(() => buildLanding(edited, readRepo('CHANGELOG.md'))).toThrow('terminal("rekcod snap save seeded") matched 0');
    const twice = new Readme('# x\n\n```sh\nrekcod ps\n```\n\n```sh\nrekcod ps\n```\n');
    expect(() => twice.snippet('rekcod ps')).toThrow('matched 2');
  });

  test('figures are read from the captured benchmark block', () => {
    const block = landing.measured.terminal.block.value;
    for (const f of landing.figures) expect(block).toContain(f.value.replace(/ (s|ms|µs)$/, '').replace(/,/g, ''));
    const edited = new Readme(readme.markdown.replace('p50 14.4 ms', 'p50 9.9 ms'));
    expect(buildLanding(edited, readRepo('CHANGELOG.md')).figures[1].value).toBe('9.9 ms');
  });

  test('every capability section has an id, a title and 1 to 3 sentences naming a command', () => {
    for (const c of landing.capabilities) {
      expect(c.id).toMatch(/^[a-z0-9-]+$/);
      expect(c.title.split(' ').length).toBeLessThanOrEqual(6);
      expect(c.title).not.toMatch(/\.$/);
      expect(c.text).toMatch(/`[^`]+`/);
      const sentences = c.text.replace(/\[[^\]]*\]\([^)]*\)\.?/g, '').replace(/`[^`]*`/g, 'x').split(/\.\s/).filter((s) => s.trim());
      expect(sentences.length).toBeGreaterThanOrEqual(1);
      expect(sentences.length).toBeLessThanOrEqual(3);
    }
    expect(landing.h1.split(' ').length).toBeLessThanOrEqual(8);
  });

  test('every /docs/ anchor the page links to is a README section', () => {
    const anchors = JSON.stringify(landing).match(/\/docs\/#[a-z0-9-]+/g) ?? [];
    expect(anchors.length).toBeGreaterThan(5);
    for (const a of anchors) expect(readme.hasSection(a.slice(7))).toBe(true);
    for (const c of landing.capabilities) expect(readme.hasSection(c.id)).toBe(true);
  });

  test('the hero diagram uses the README diagram\'s labels and a captured container name', () => {
    const block = readme.blocks.find((b) => b.lang === 'mermaid' && b.value.includes('accTitle: Local Docker architecture'))!.value;
    const d = landing.diagram;
    for (const label of [d.mac, d.guest, d.clients, d.daemon, d.agent, d.engine, d.containerd, d.runc, d.containers, d.socket, d.vsock]) expect(block).toContain(label);
    expect(readme.markdown).toContain(`$ rekcod exec ${d.container} `);
    const broken = new Readme(readme.markdown.replace('daemon["rekcod daemon"]', 'daemon'));
    expect(() => buildLanding(broken, readRepo('CHANGELOG.md'))).toThrow('has no daemon');
  });

  test('.vercelignore uploads every repository file the build reads', () => {
    const ignore = readRepo('.vercelignore').split('\n');
    for (const f of ['README.md', 'CHANGELOG.md']) expect(ignore).toContain(`!${f}`);
  });

  test('version is the newest dated CHANGELOG release', () => {
    expect(latestRelease('## [0.3.0] - Unreleased\n\n## [0.2.1] - 2026-10-12\n')).toEqual({ version: '0.2.1', date: '2026-10-12' });
    expect(landing.version).toMatch(/^\d+\.\d+\.\d+/);
  });

  test('copy has no em dashes', () => {
    expect(JSON.stringify(landing)).not.toContain('\u2014');
  });
});

describe('generated Markdown and helpers', () => {
  test('page, llms.txt and AGENTS.md carry every section and absolute links', () => {
    const page = pageMarkdown(landing);
    const llms = llmsTxt(landing);
    for (const c of landing.capabilities) {
      expect(page).toContain(`## ${c.title}`);
      expect(llms).toContain(`## ${c.title}`);
    }
    expect(llms).toContain(url('/docs/'));
    expect(llms).not.toMatch(/\]\(\//);
    expect(agentsMd(landing)).toContain('| `rekcod start` |');
  });

  test('url() gives the clean canonical form', () => {
    expect(url('/')).toBe('https://rekcod.vercel.app/');
    expect(url('/index.html')).toBe('https://rekcod.vercel.app/');
    expect(url('/docs/')).toBe('https://rekcod.vercel.app/docs/');
  });

  test('meta description stays within 160 characters at a sentence end', () => {
    expect(landing.description.length).toBeLessThanOrEqual(160);
    expect(landing.description).toMatch(/\.$/);
    expect(metaDescription('One. ' + 'x'.repeat(200) + '.')).toBe('One.');
  });

  test('captured output is escaped, and diff lines are marked', () => {
    const html = consoleHtml('$ echo <img onerror=x>\n<b>\ndiff --git a/f b/f\n-old\n+new');
    expect(html).not.toContain('<img');
    expect(html).not.toContain('<b>');
    expect(html).toContain('class="ln del">-old');
    expect(html).toContain('class="ln add">+new');
  });
});
