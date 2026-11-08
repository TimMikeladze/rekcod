import { describe, expect, test } from 'bun:test';
import { readFile } from 'node:fs/promises';
import {
  compileMarkdown,
  getDocuments,
  getNavigation,
  landingParts,
  resolveLink,
  routeFor,
} from './content';

describe('repository Markdown', () => {
  test('landing content follows the README without separate website copy', async () => {
    const markdown = await readFile(new URL('../../../README.md', import.meta.url), 'utf8');
    const parts = landingParts(markdown);
    expect(parts.title).toBe('rekcod');
    expect(parts.intro).toContain('Docker Desktop alternative');
    expect(parts.body).toContain('rekcod start');
    expect(parts.intro + parts.body).toContain('All docs');
    const edited = landingParts('# Changed name\n\nChanged introduction.\n\n## Install now\n\n```sh\nnew-command\n```');
    expect(edited.title).toBe('Changed name');
    expect(edited.intro).toContain('Changed introduction.');
    expect(edited.body).toContain('new-command');
  });

  test('maps readmes and nested documents to stable routes', () => {
    expect(routeFor('README.md')).toBe('/');
    expect(routeFor('docs/README.md')).toBe('/docs/');
    expect(routeFor('docs/getting-started.md')).toBe('/docs/getting-started/');
    expect(routeFor('examples/github-runner/README.md')).toBe('/docs/examples/github-runner/');
    expect(routeFor('docs/new-feature.md')).toBe('/docs/new-feature/');
  });

  test('resolves relative documents, fragments and source files', () => {
    const files = new Set(['README.md', 'docs/README.md', 'docs/getting-started.md']);
    expect(resolveLink('../README.md#how-it-works', 'docs/getting-started.md', files)).toBe('/#how-it-works');
    expect(resolveLink('getting-started.md', 'docs/README.md', files)).toBe('/docs/getting-started/');
    expect(resolveLink('../scripts/install.sh', 'docs/getting-started.md', files)).toBe('https://github.com/TimMikeladze/rekcod/blob/main/scripts/install.sh');
    expect(resolveLink('#install', 'docs/getting-started.md', files)).toBe('#install');
    expect(resolveLink('https://example.com/file.md', 'README.md', files)).toBe('https://example.com/file.md');
    expect(resolveLink('missing.md', 'README.md', files)).toBe('https://github.com/TimMikeladze/rekcod/blob/main/missing.md');
    expect(resolveLink('getting-started.md?view=all#install', 'docs/README.md', files)).toBe('/docs/getting-started/?view=all#install');
    expect(resolveLink('getting-started.md#L110', 'docs/README.md', files)).toBe('https://github.com/TimMikeladze/rekcod/blob/main/docs/getting-started.md#L110');
  });

  test('renders GFM and GitHub heading IDs while removing executable HTML', async () => {
    const rendered = await compileMarkdown('# Guide\n\n## Run `rekcod x`\n\n## Run `rekcod x`\n\n| A | B |\n| - | - |\n| 1 | 2 |\n\n<script>alert(1)</script><img src="x" onerror="alert(1)">\n\n[bad](javascript:alert(1))', 'docs/example.md', new Set());
    expect(rendered.html).toContain('<table');
    expect(rendered.headings.map((heading) => heading.id)).toEqual(['guide', 'run-rekcod-x', 'run-rekcod-x-1']);
    expect(rendered.html).not.toContain('<script');
    expect(rendered.html).not.toContain('onerror');
    expect(rendered.html).not.toContain('javascript:');
  });

  test('discovers repository documents and derives sidebar groups from the index', async () => {
    const documents = await getDocuments();
    expect(documents.some((doc) => doc.file === 'docs/getting-started.md')).toBe(true);
    expect(documents.some((doc) => doc.file === 'examples/github-runner/README.md')).toBe(true);
    expect(new Set(documents.map((doc) => doc.url)).size).toBe(documents.length);
    const navigation = getNavigation(documents);
    expect(navigation.find((group) => group.title === 'Using rekcod')?.items.some((item) => item.url === '/docs/getting-started/')).toBe(true);
    for (const group of navigation) {
      for (const item of group.items) expect(documents.some((doc) => doc.url === item.url)).toBe(true);
    }
  });
});
