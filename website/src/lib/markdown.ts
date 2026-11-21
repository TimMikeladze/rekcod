import type { Landing } from './landing';
import { url, repository } from './landing';
import type { Snippet, Terminal } from './readme';

// The landing model as Markdown: /index.md, /llms.txt and /AGENTS.md.

function fence(lang: string, value: string): string {
  const ticks = '`'.repeat(Math.max(3, ...(value.match(/`+/g) ?? []).map((m) => m.length + 1)));
  return `${ticks}${lang}\n${value}\n${ticks}`;
}

function absolute(markdown: string): string {
  return markdown
    .replace(/\]\((\/[^)]*)\)/g, (_, p) => `](${url(p)})`)
    // A repository file the README links to by its relative path.
    .replace(/\]\(([A-Z][\w.-]*\.md)\)/g, (_, f) => `](${repository}/blob/main/${f})`);
}

function artifact(a: Snippet | Terminal): string {
  if (a.kind === 'terminal') {
    const lines = a.block.value.split('\n');
    return fence('console', lines.slice(lines.indexOf(`$ ${a.command}`)).join('\n'));
  }
  return fence(a.block.lang, a.block.value);
}

function sections(l: Landing): string[] {
  return l.capabilities.map(({ id, title, text, example, guide }) => [
    `## ${title}`,
    absolute(text),
    artifact(example),
    `[${guide}](${url(`/docs/#${id}`)})`,
  ].join('\n\n'));
}

/** The explanation sections between the quickstart and the commands. */
function explanation(l: Landing): string[] {
  return [
    `## ${l.why.title}`,
    l.why.text,
    `## ${l.how.title}`,
    l.how.crossings.map((c) => `- **${c.title}**${c.measure ? ` (${c.measure})` : ''}: ${c.text} [${c.link}](${url(c.href)})`).join('\n'),
    `## ${l.files.title}`,
    l.files.text,
    l.files.steps.map((s) => `- ${s.from === s.to ? s.from : `${s.from} to ${s.to}`}: ${s.text}`).join('\n'),
    `## ${l.decisions.title}`,
    l.decisions.items.map((d) => `- **${d.title}**: ${d.text} [Details](${url(d.href)})`).join('\n'),
  ];
}

function switching(l: Landing): string[] {
  const s = l.switching;
  return [`## ${s.title}`, s.text, artifact(s.example), `[${s.guide}](${url(`/docs/#${s.id}`)})`];
}

function limitations(l: Landing): string[] {
  return [`## ${l.limitations.title}`, absolute(l.limitations.text), absolute(l.limitations.list)];
}

function start(l: Landing): string[] {
  return [`## ${l.start.title}`, artifact(l.start.example), l.start.text, `[Installation details](${url(l.start.href)})`];
}

export function pageMarkdown(l: Landing): string {
  return [
    `# ${l.h1}`,
    absolute(l.lede),
    ...start(l),
    ...explanation(l),
    ...sections(l),
    ...switching(l),
    `## ${l.measured.title}`,
    l.measured.text,
    l.figures.map((f) => `- ${f.value} ${f.label}`).join('\n'),
    fence('console', l.measured.terminal.block.value),
    l.measured.method,
    `[Benchmark commands and output](${url(l.measured.href)})`,
    ...limitations(l),
    '## More in the docs',
    l.guides.map(({ label, href }) => `- [${label}](${url(href)})`).join('\n'),
  ].join('\n\n') + '\n';
}

export function llmsTxt(l: Landing): string {
  return [
    `# ${l.name}`,
    `> ${l.description}`,
    absolute(l.lede),
    `Current release v${l.version}.`,
    ...start(l),
    ...explanation(l),
    ...sections(l),
    ...switching(l),
    ...limitations(l),
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
    ...start(l),
    '## Commands',
    'Run `rekcod COMMAND --help` for each command\'s options.',
    l.agents.commands.markdown,
    '## Mistakes that break it',
    l.agents.mistakes.map((m) => `- ${m}`).join('\n'),
    '## Sandboxes for agents',
    'Give an agent `rekcod mcp` (MCP on stdio) or `rekcod sandbox serve`. It works on a copy of the project in its own VM; you review `rekcod sandbox diff` and apply it.',
    fence('console', l.agents.sandbox.block.value),
  ].join('\n\n') + '\n';
}
