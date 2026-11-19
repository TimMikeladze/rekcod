import type { Root, Code, Table, Heading, Paragraph, RootContent } from 'mdast';
import { toString } from 'mdast-util-to-string';
import GithubSlugger from 'github-slugger';
import { unified } from 'unified';
import remarkParse from 'remark-parse';
import remarkGfm from 'remark-gfm';

// Resolves the landing page's references into README.md. Every resolver throws unless
// exactly one thing matches, so editing an example in the README breaks the build
// instead of leaving the page quoting something that no longer exists.

export interface Block { lang: string; value: string; section: string; line: number }
export interface Terminal { kind: 'terminal'; block: Block; command: string }
export interface Snippet { kind: 'snippet'; block: Block; label: string }
export interface TableRef { kind: 'table'; markdown: string; section: string }
export type Artifact = Terminal | Snippet | TableRef;

const parser = unified().use(remarkParse).use(remarkGfm);

/** A line of a block without a trailing `# comment`, for matching. */
function bare(line: string): string {
  return line.replace(/\s+#\s.*$/, '').trim();
}

export class Readme {
  readonly blocks: Block[] = [];
  readonly tables: { header: string[]; markdown: string; section: string }[] = [];
  readonly paragraphs: { markdown: string; text: string; section: string }[] = [];
  readonly slugs = new Set<string>();
  readonly title: string;
  readonly intro: string;

  constructor(readonly markdown: string) {
    const tree = parser.parse(markdown) as Root;
    const slugger = new GithubSlugger();
    let section = '';
    let title = '';
    let intro = '';
    for (const node of tree.children as RootContent[]) {
      if (node.type === 'heading') {
        const text = toString(node as Heading);
        if ((node as Heading).depth === 1 && !title) { title = text; continue; }
        section = slugger.slug(text);
        this.slugs.add(section);
      } else if (node.type === 'code') {
        const code = node as Code;
        this.blocks.push({ lang: code.lang ?? '', value: code.value, section, line: code.position!.start.line });
      } else if (node.type === 'table') {
        const table = node as Table;
        this.tables.push({
          header: table.children[0].children.map((cell) => toString(cell)),
          markdown: this.source(node),
          section,
        });
      } else if (node.type === 'paragraph') {
        if (!section && !intro) intro = this.source(node);
        this.paragraphs.push({ markdown: this.source(node), text: toString(node as Paragraph), section });
      }
    }
    this.title = title;
    this.intro = intro;
  }

  private source(node: RootContent): string {
    return this.markdown.slice(node.position!.start.offset, node.position!.end.offset);
  }

  private one<T>(found: T[], what: string): T {
    if (found.length !== 1) throw new Error(`README.md: ${what} matched ${found.length} blocks, expected exactly one`);
    return found[0];
  }

  /** The `console` block holding the line `$ command`: a captured run. */
  terminal(command: string): Terminal {
    const block = this.one(
      this.blocks.filter((b) => b.lang === 'console' && b.value.split('\n').includes(`$ ${command}`)),
      `terminal("${command}")`,
    );
    return { kind: 'terminal', block, command };
  }

  /** The block with a line equal to `line` (a trailing `# comment` is ignored). */
  snippet(line: string, label?: string): Snippet {
    const block = this.one(
      this.blocks.filter((b) => b.lang !== 'console' && b.lang !== 'mermaid' && b.value.split('\n').some((l) => bare(l) === line || l.trim() === line)),
      `snippet("${line}")`,
    );
    return { kind: 'snippet', block, label: label ?? defaultLabel(block) };
  }

  /** The one line, across every block, that contains `text`. */
  line(text: string): string {
    const lines = this.blocks.filter((b) => b.lang !== 'console').flatMap((b) => b.value.split('\n')).filter((l) => l.includes(text));
    return this.one(lines, `line("${text}")`).trim();
  }

  /** The table whose header cells are `header`, in order. */
  table(...header: string[]): TableRef {
    const found = this.one(this.tables.filter((t) => t.header.join('|') === header.join('|')), `table(${header.join(', ')})`);
    return { kind: 'table', markdown: found.markdown, section: found.section };
  }

  /** The paragraph that starts with `start` (compared as rendered text). */
  paragraph(start: string): string {
    return this.one(this.paragraphs.filter((p) => p.text.startsWith(start)), `paragraph("${start}")`).markdown;
  }

  /** The first capture group of `pattern` in a block, for headline figures. */
  figure(block: Block, pattern: RegExp): string {
    const match = block.value.match(pattern);
    if (!match?.[1]) throw new Error(`README.md: ${pattern} found nothing in the block at line ${block.line}`);
    return match[1];
  }

  /**
   * Node and edge labels of the Mermaid block with this accTitle, so a drawing of the
   * diagram uses the README's own names.
   */
  diagram(accTitle: string): { nodes: Record<string, string>; edges: Record<string, string> } {
    const block = this.one(this.blocks.filter((b) => b.lang === 'mermaid' && b.value.includes(`accTitle: ${accTitle}`)), `diagram("${accTitle}")`);
    const nodes: Record<string, string> = {};
    const edges: Record<string, string> = {};
    for (const [, id, label] of block.value.matchAll(/(\w+)\["([^"]+)"\]/g)) nodes[id] = label;
    for (const [, from, label, to] of block.value.matchAll(/(\w+)(?:\["[^"]*"\])?\s*-->\|([^|]+)\|\s*(\w+)/g)) edges[`${from}-${to}`] = label;
    return { nodes, edges };
  }

  hasSection(slug: string): boolean {
    return this.slugs.has(slug);
  }
}

function defaultLabel(block: Block): string {
  if (block.lang === 'yaml') return 'compose.yaml';
  if (block.lang === 'toml') return 'rekcod.toml';
  return 'terminal';
}

/** The newest CHANGELOG.md release with a date, which is what install.sh installs. */
export function latestRelease(changelog: string): { version: string; date: string } {
  const match = changelog.match(/^## \[(\d+\.\d+\.\d+[^\]]*)\] - (\d{4}-\d{2}-\d{2})$/m);
  if (!match) throw new Error('CHANGELOG.md has no dated release');
  return { version: match[1], date: match[2] };
}
