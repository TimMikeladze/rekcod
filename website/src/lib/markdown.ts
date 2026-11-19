import type { Landing } from './landing';
import { url, repository } from './landing';
import type { Artifact } from './readme';

// The landing model as Markdown: /index.md, /llms.txt and /AGENTS.md.

function fence(lang: string, value: string): string {
  const ticks = '`'.repeat(Math.max(3, ...(value.match(/`+/g) ?? []).map((m) => m.length + 1)));
  return `${ticks}${lang}\n${value}\n${ticks}`;
}

function absolute(markdown: string): string {
  return markdown.replace(/\]\((\/[^)]*)\)/g, (_, p) => `](${url(p)})`);
}

function artifact(a: Artifact): string {
  if (a.kind === 'table') return a.markdown;
  if (a.kind === 'terminal') {
    const lines = a.block.value.split('\n');
    return fence('console', lines.slice(lines.indexOf(`$ ${a.command}`)).join('\n'));
  }
  return fence(a.block.lang, a.block.value);
}

function sections(l: Landing): string[] {
  return l.capabilities.map((c) => [`## ${c.title}`, absolute(c.text), ...c.show.map(artifact), ...(c.aside ? [absolute(c.aside)] : [])].join('\n\n'));
}

export function pageMarkdown(l: Landing): string {
  return [
    `# ${l.h1}`,
    absolute(l.lede),
    fence('sh', l.install),
    `Currently v${l.version}.`,
    `## ${l.split.title}`,
    fence('console', l.split.terminal.block.value),
    ...sections(l),
    `## ${l.measured.title}`,
    l.measured.text,
    ...l.figures.map((f) => `- ${f.value}: ${f.label}`),
    fence('console', l.measured.terminal.block.value),
    `## ${l.start.title}`,
    absolute(l.start.text),
    ...l.start.panels.map(artifact),
    `Full documentation: ${url('/docs/')}`,
  ].join('\n\n') + '\n';
}

export function llmsTxt(l: Landing): string {
  return [
    `# ${l.name}`,
    `> ${l.description}`,
    absolute(l.lede),
    `Install (v${l.version}):`,
    fence('sh', l.install),
    ...sections(l),
    '## Links',
    [
      `- [Documentation (the README in full)](${url('/docs/')})`,
      `- [How to use rekcod as an agent](${url('/AGENTS.md')})`,
      `- [This page as Markdown](${url('/index.md')})`,
      `- [Repository](${repository})`,
    ].join('\n'),
  ].join('\n\n') + '\n';
}

export function agentsMd(l: Landing): string {
  return [
    '# Using rekcod',
    `rekcod runs Docker Engine in a Linux VM on Apple Silicon Macs. It is a single \`rekcod\` binary; anything it doesn't know runs as \`docker COMMAND\` against its engine. Docs: ${url('/docs/')}`,
    '## Install and run',
    fence(l.agents.minimal.block.lang, l.agents.minimal.block.value),
    '## Commands',
    'Run `rekcod COMMAND --help` for each command\'s options.',
    l.agents.commands.markdown,
    '## Mistakes that break it',
    l.agents.mistakes.map((m) => `- ${m}`).join('\n'),
    '## Sandboxes for agents',
    'Give an agent `rekcod mcp` (MCP on stdio) or `rekcod sandbox serve`. It works on a copy of the project in its own VM; you review `rekcod sandbox diff` and apply it.',
    fence('console', l.split.terminal.block.value),
  ].join('\n\n') + '\n';
}
