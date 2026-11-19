import type { Block } from './readme';
import { compileMarkdown } from './content';

// Turns resolved README references into HTML for the landing page. Markup only, no copy.

const files = new Set(['README.md']);

export function escape(text: string): string {
  return text.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
}

/** Inline Markdown (copy with `code` and links) without the wrapping paragraph. */
export async function inline(markdown: string): Promise<string> {
  const { html } = await compileMarkdown(markdown, 'README.md', files);
  return html.replace(/^<p>/, '').replace(/<\/p>\s*$/, '');
}

export async function block(markdown: string): Promise<string> {
  return (await compileMarkdown(markdown, 'README.md', files)).html;
}

/** A fenced block highlighted by Shiki in both themes. */
export async function code(b: Block): Promise<string> {
  const fence = '`'.repeat(Math.max(3, ...(b.value.match(/`+/g) ?? []).map((m) => m.length + 1)));
  return block(`${fence}${b.lang || 'text'}\n${b.value}\n${fence}`);
}

/**
 * A captured run: `$` lines are commands, the rest is output, and the lines of a
 * unified diff in the output are marked as added, deleted or hunk headers.
 */
export function consoleHtml(value: string, from = 0, to?: number): string {
  const lines = value.split('\n').slice(from, to);
  let inDiff = false;
  const out = lines.map((line) => {
    if (line.startsWith('$ ')) {
      inDiff = false;
      return `<span class="ln cmd"><span class="ps" aria-hidden="true">$ </span>${escape(line.slice(2))}</span>`;
    }
    if (line.startsWith('diff --git')) inDiff = true;
    let cls = 'out';
    if (inDiff) {
      if (line.startsWith('@@')) cls = 'hunk';
      else if (line.startsWith('+') && !line.startsWith('+++')) cls = 'add';
      else if (line.startsWith('-') && !line.startsWith('---')) cls = 'del';
      else if (/^(diff|---|\+\+\+)/.test(line)) cls = 'meta';
    }
    if (line.startsWith('rekcod: ') || line.startsWith('rekcod ready')) { inDiff = false; cls = 'note'; }
    return `<span class="ln ${cls}">${escape(line) || ' '}</span>`;
  });
  return `<pre class="console" tabindex="0"><code>${out.join('\n')}</code></pre>`;
}

/** Index of the line `$ command` in a console block. */
export function commandIndex(value: string, command: string): number {
  return value.split('\n').indexOf(`$ ${command}`);
}
