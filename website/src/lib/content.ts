import { readFile } from 'node:fs/promises';
import path from 'node:path';
import type { Root as HastRoot } from 'hast';
import type { Root as MarkdownRoot } from 'mdast';
import { toText } from 'hast-util-to-text';
import { toString } from 'mdast-util-to-string';
import { unified } from 'unified';
import remarkParse from 'remark-parse';
import remarkGfm from 'remark-gfm';
import remarkRehype from 'remark-rehype';
import rehypeRaw from 'rehype-raw';
import rehypeSanitize from 'rehype-sanitize';
import rehypeSlug from 'rehype-slug';
import rehypeShiki from '@shikijs/rehype';
import rehypeStringify from 'rehype-stringify';
import { visit } from 'unist-util-visit';

export const repository = 'https://github.com/TimMikeladze/rekcod';
// Astro bundles this module into dist before prerendering; cwd stays at website/.
const root = path.resolve(process.cwd(), '..');
const parser = unified().use(remarkParse).use(remarkGfm);

export interface Document {
  file: string;
  url: string;
  title: string;
  description: string;
  markdown: string;
  text: string;
}

export interface Heading {
  depth: number;
  id: string;
  text: string;
}

export function routeFor(file: string): string {
  if (file === 'README.md') return '/';
  const slug = file.replace(/^docs\//, '').replace(/(?:^|\/)README\.md$/, '').replace(/\.md$/i, '').toLowerCase();
  return `/docs/${slug ? `${slug}/` : ''}`;
}

export function resolveLink(href: string, file: string, files: Set<string>): string {
  if (!href || /^(?:[a-z][a-z\d+.-]*:|#|\/\/)/i.test(href)) return href;
  const suffixIndex = href.search(/[?#]/);
  const pathname = suffixIndex < 0 ? href : href.slice(0, suffixIndex);
  const suffix = suffixIndex < 0 ? '' : href.slice(suffixIndex);
  let decoded: string;
  try { decoded = decodeURIComponent(pathname); } catch { decoded = pathname; }
  const target = path.posix.normalize(path.posix.join(path.posix.dirname(file), decoded));
  if (files.has(target) && !/#L\d+(?:-L\d+)?$/.test(suffix)) return routeFor(target) + suffix;
  return `${repository}/blob/main/${target.split('/').map(encodeURIComponent).join('/')}${suffix}`;
}

export function landingParts(markdown: string): { title: string; intro: string; body: string } {
  const tree = parser.parse(markdown) as MarkdownRoot;
  const title = tree.children.find((node) => node.type === 'heading' && node.depth === 1);
  const firstSection = tree.children.find((node) => node.type === 'heading' && node.depth === 2);
  const start = title?.position?.end.offset ?? 0;
  const end = firstSection?.position?.start.offset ?? markdown.length;
  return {
    title: title ? toString(title) : 'rekcod',
    intro: markdown.slice(start, end).trim(),
    body: markdown.slice(end).trim(),
  };
}

export async function getDocuments(): Promise<Document[]> {
  const files = ['README.md'];
  const documents = await Promise.all(files.sort().map(async (file) => {
    const markdown = await readFile(path.join(root, file), 'utf8');
    const tree = parser.parse(markdown) as MarkdownRoot;
    const heading = tree.children.find((node) => node.type === 'heading');
    const paragraph = tree.children.find((node) => node.type === 'paragraph');
    return {
      file,
      url: routeFor(file),
      title: heading ? toString(heading) : path.basename(file, '.md').replace(/-/g, ' '),
      description: paragraph ? toString(paragraph).slice(0, 200) : '',
      markdown,
      text: toString(tree).replace(/\s+/g, ' '),
    };
  }));
  const urls = new Set<string>();
  for (const doc of documents) {
    if (urls.has(doc.url)) throw new Error(`Duplicate Markdown route: ${doc.file} → ${doc.url}`);
    urls.add(doc.url);
  }
  return documents;
}

const renderer = unified()
  .use(remarkParse)
  .use(remarkGfm)
  .use(remarkRehype, { allowDangerousHtml: true })
  .use(rehypeRaw)
  .use(rehypeSanitize)
  .use(rehypeSlug)
  .use(() => (tree: HastRoot) => {
    visit(tree, 'element', (node) => {
      if (node.tagName !== 'pre') return;
      const code = node.children[0];
      if (code?.type !== 'element' || code.tagName !== 'code') return;
      const classes = code.properties.className;
      if (!Array.isArray(classes) || !classes.includes('language-mermaid')) return;
      node.properties.className = ['mermaid'];
      node.children = [{ type: 'text', value: toText(code, { whitespace: 'pre' }) }];
    });
  })
  .use(rehypeShiki, { theme: 'github-light-high-contrast', fallbackLanguage: 'text' });

export async function compileMarkdown(markdown: string, file: string, files: Set<string>) {
  const headings: Heading[] = [];
  const processor = renderer()
    .use(() => (tree: HastRoot) => {
      visit(tree, 'element', (node) => {
        if (/^h[1-6]$/.test(node.tagName)) {
          headings.push({ depth: Number(node.tagName[1]), id: String(node.properties.id), text: toText(node) });
        }
        if (node.tagName === 'a' && typeof node.properties.href === 'string') {
          node.properties.href = resolveLink(node.properties.href, file, files);
        }
        if (node.tagName === 'img' && typeof node.properties.src === 'string' && !/^(?:[a-z]+:|\/\/)/i.test(node.properties.src)) {
          const target = path.posix.normalize(path.posix.join(path.posix.dirname(file), node.properties.src));
          node.properties.src = `https://raw.githubusercontent.com/TimMikeladze/rekcod/main/${target}`;
          node.properties.loading = 'lazy';
        }
        if (node.tagName === 'table') node.properties.tabIndex = 0;
      });
    })
    .use(rehypeStringify);
  const result = await processor.process(markdown);
  return { html: String(result), headings };
}
