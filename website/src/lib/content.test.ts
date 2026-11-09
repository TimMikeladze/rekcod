import { describe, expect, test } from 'bun:test';
import { readFile } from 'node:fs/promises';
import {
  compileMarkdown,
  getDocuments,
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
    expect(parts.intro + parts.body).toContain('[MIT](LICENSE)');
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

  test('uses only the root README as website content', async () => {
    const documents = await getDocuments();
    expect(documents.map((doc) => doc.file)).toEqual(['README.md']);
    expect(documents[0].url).toBe('/');
  });

  test('preserves Mermaid source for rendering and escapes embedded HTML', async () => {
    const rendered = await compileMarkdown('```mermaid\nflowchart TD\n  A["<img src=x onerror=alert(1)>"] --> B["Linux & Docker"]\n```\n\n```sh\nrekcod start\n```', 'README.md', new Set());
    expect(rendered.html).toContain('<pre class="mermaid">flowchart TD\n');
    expect(rendered.html).toMatch(/(?:&lt;|&#x3C;)img /i);
    expect(rendered.html).not.toContain('<img');
    expect(rendered.html).toContain('class="shiki ');
  });
});
