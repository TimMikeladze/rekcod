import { readFileSync } from 'node:fs';
import path from 'node:path';
import { Readme, latestRelease, type Artifact, type Block, type Snippet, type Terminal } from './readme';

// The landing page model: copy, section order, which README block each section shows and
// the link table. No markup. Copy is written here; every example is a README reference.

export const origin = 'https://rekcod.vercel.app';
export const repository = 'https://github.com/TimMikeladze/rekcod';
export const themeKey = 'rekcod-theme';

/** Absolute URL for a site path, the way people paste it: `/`, `/docs/`, never index.html. */
export function url(pathname: string): string {
  return new URL(pathname.replace(/index\.html$/, ''), origin).toString();
}

export type GlyphName = 'copy' | 'check' | 'book' | 'arrow' | 'sun' | 'moon' | 'monitor' | 'terminal' | 'file' | 'bot';
export type BrandName = 'github' | 'x' | 'linkedin' | 'discord' | 'apple' | 'docker' | 'linux' | 'homebrew' | 'bun'
  | 'nodedotjs' | 'python' | 'uv' | 'rust' | 'tailscale' | 'cloudflare' | 'githubactions' | 'modelcontextprotocol';

export interface Link {
  label: string;
  href: string;
  icon: BrandName | 'text';
  where: ('header' | 'footer')[];
}

export const links: Link[] = [
  { label: 'Docs', href: '/docs/', icon: 'text', where: ['header'] },
  { label: 'Benchmarks', href: '/#benchmarks', icon: 'text', where: ['header'] },
  { label: 'Changelog', href: `${repository}/blob/main/CHANGELOG.md`, icon: 'text', where: ['header'] },
  { label: 'TimMikeladze/rekcod on GitHub', href: 'repo', icon: 'github', where: ['header', 'footer'] },
  { label: 'linesofcode on X', href: 'https://x.com/linesofcode', icon: 'x', where: ['header', 'footer'] },
  { label: 'linesofcode on LinkedIn', href: 'https://www.linkedin.com/in/tim-mikeladze', icon: 'linkedin', where: ['header', 'footer'] },
  { label: 'linesofcode on Discord', href: 'https://discord.com/users/linesofcode', icon: 'discord', where: ['footer'] },
];

export function href(link: { href: string }): string {
  return link.href === 'repo' ? repository : link.href;
}

export interface FooterColumn { title: string; links: { label: string; href: string }[] }

export const footerColumns: FooterColumn[] = [
  { title: 'rekcod', links: [
    { label: 'Home', href: '/' },
    { label: 'Docs', href: '/docs/' },
    { label: 'Benchmarks', href: '/#benchmarks' },
    { label: 'Changelog', href: `${repository}/blob/main/CHANGELOG.md` },
    { label: 'Releases', href: `${repository}/releases` },
  ] },
  { title: 'Learn', links: [
    { label: 'Run your first container', href: '/docs/#run-your-first-container' },
    { label: 'How it works', href: '/docs/#how-it-works' },
    { label: 'Command reference', href: '/docs/#command-reference' },
    { label: 'When something breaks', href: '/docs/#when-something-breaks' },
    { label: 'Limitations', href: '/docs/#limitations' },
  ] },
  { title: 'Self-hosting', links: [
    { label: 'Deploy an app', href: '/docs/#deploy-an-app' },
    { label: 'Add and remove nodes', href: '/docs/#add-and-remove-nodes' },
    { label: 'Give people access', href: '/docs/#give-people-access' },
    { label: 'Back up the controller', href: '/docs/#back-up-the-controller' },
  ] },
  { title: 'Agents', links: [
    { label: 'llms.txt', href: '/llms.txt' },
    { label: 'AGENTS.md', href: '/AGENTS.md' },
    { label: 'Page as Markdown', href: '/index.md' },
    { label: 'MCP server', href: '/docs/#let-an-agent-drive-sandboxes' },
  ] },
  { title: 'Community', links: [
    { label: 'GitHub', href: repository },
    { label: 'Issues', href: `${repository}/issues` },
    { label: 'Contributing', href: `${repository}/blob/main/CONTRIBUTING.md` },
    { label: 'Security policy', href: `${repository}/blob/main/SECURITY.md` },
  ] },
  { title: 'Legal', links: [
    { label: 'MIT License', href: `${repository}/blob/main/LICENSE` },
    { label: 'Third-party notices', href: `${repository}/blob/main/NOTICE` },
    { label: 'linesofcode.dev', href: 'https://linesofcode.dev' },
  ] },
];

export interface Figure { value: string; label: string }

export interface Capability {
  id: string;
  title: string;
  /** 1 to 3 sentences of inline Markdown naming the exact command or option. */
  text: string;
  show: Artifact[];
  /** One sentence offering the adjacent thing, shown after the section. */
  aside?: string;
}

export interface Tab { label: string; snippet: Snippet }
export interface Guide { title: string; text: string; href: string; snippet: Snippet }
export interface Principle { title: string; text: string }
export interface Brand { name: BrandName; label: string }

const root = path.resolve(process.cwd(), '..');

export function readRepo(file: string): string {
  return readFileSync(path.join(root, file), 'utf8');
}

export function buildLanding(readme: Readme, changelog: string) {
  const release = latestRelease(changelog);
  const install = readme.line('scripts/install.sh | bash');
  const sandbox = readme.terminal('rekcod sandbox diff "$id"');
  const bench = {
    start: readme.terminal('rekcod start --no-switch'),
  };
  const benchBlock: Block = bench.start.block;
  const figures: Figure[] = [
    { value: `${readme.figure(benchBlock, /rekcod ready in ([\d.]+)s/)} s`, label: 'from a stopped VM to Docker answering' },
    { value: `${readme.figure(benchBlock, /inotify round trip .*?p50 ([\d.]+) ms/)} ms`, label: 'Mac edit to inotify in a container and back, p50' },
    { value: `${readme.figure(benchBlock, /mac p50\s+([\d.]+) µs/)} µs`, label: 'TCP round trip through a published port, p50' },
    { value: Number(readme.figure(benchBlock, /(\d+) req\/s/)).toLocaleString('en-US'), label: 'requests a second over 8 connections from the Mac' },
  ];

  const capabilities: Capability[] = [
    {
      id: 'run-the-project-you-already-have',
      title: 'Bring your Compose project',
      text: 'Run `rekcod compose up -d` from the project directory, or keep using `docker compose` with the `rekcod` context. rekcod runs the unmodified upstream `dockerd` with its embedded BuildKit, so builds, Compose and SDKs talk to Docker itself. [Run the project you already have](/docs/#run-the-project-you-already-have).',
      show: [readme.snippet('rekcod ps')],
    },
    {
      id: 'keep-dependency-writes-inside-linux',
      title: 'node_modules stays in Linux',
      text: 'Label a Compose service with `dev.rekcod.local` and the paths to keep in the VM. rekcod mounts a named volume at each one, so installs and build output live on ext4 while your source stays editable on the Mac.',
      show: [readme.snippet('dev.rekcod.local: "node_modules,.next"'), readme.terminal("rekcod exec shop-web-1 sh -c 'mount | grep /app'")],
    },
    {
      id: 'restore-your-database-to-a-known-state',
      title: 'Snapshot your database',
      text: '`rekcod snap save` copies a Compose project\'s named volumes after you migrate and seed. `rekcod snap restore` puts them back and keeps what it replaced as the `pre-restore` snapshot. `--push` and `--pull` move snapshots through S3-compatible storage.',
      show: [readme.terminal('rekcod snap save seeded')],
    },
    {
      id: 'run-project-tools-without-installing-them-on-your-mac',
      title: 'Toolchains without installs',
      text: '`rekcod x` runs a command in a Linux image picked from the command and files such as `.nvmrc`, `.python-version` and `rust-toolchain.toml`. It mounts the directory at the same path, passes `.env` and forwards listening ports to the Mac.',
      show: [readme.snippet('rekcod x cargo test')],
      aside: 'Want it in a VM that can only see this directory? Add [`--sandbox`](/docs/#run-it-in-a-separate-vm).',
    },
    {
      id: 'let-an-agent-drive-sandboxes',
      title: 'Let agents drive sandboxes',
      text: '`rekcod mcp` serves the sandbox operations over stdio and `rekcod sandbox serve` over HTTP, for the directory they started in. Anything beyond a plain copy sandbox needs a flag from you.',
      show: [readme.snippet('rekcod mcp'), readme.table('Flag', 'Lets callers')],
    },
    {
      id: 'published-ports',
      title: 'Ports behave like Linux',
      text: 'A plain `-p 8080:80` listens on every interface of the Mac, and `-p 127.0.0.1:8080:80` on this Mac only. `rekcod status` lists each forwarded port, and a port another Mac process already holds with its error.',
      show: [readme.snippet('docker run -d -p 127.0.0.1:8080:80 nginx')],
    },
    {
      id: 'amd64-images',
      title: 'amd64 images through Rosetta',
      text: '`linux/amd64` images run in the same VM through Rosetta. A Mac without Rosetta boots without it, and `softwareupdate --install-rosetta` fixes that.',
      show: [readme.snippet('docker run --rm --platform linux/amd64 alpine uname -m')],
    },
    {
      id: 'use-another-mac-as-your-docker-engine',
      title: 'Another Mac as your engine',
      text: '`rekcod host add` sets up SSH to a Mac running rekcod and selects its Docker context, so `docker ps` lists its containers. `rekcod host forward` brings its published ports to your `localhost`.',
      show: [readme.snippet('rekcod host forward mini')],
    },
    {
      id: 'self-host-apps-on-a-mac',
      title: 'Deploy apps to a Mac',
      text: '`rekcod host add --server` installs a controller, worker node, HTTP edge and RustFS on another Mac. Commit a `rekcod.toml`, push, and `rekcod app deploy` builds that commit in fresh VMs and switches traffic once their health checks pass.',
      show: [readme.snippet('name = "blog"'), readme.snippet('rekcod app deploy --remote mini', 'deploy')],
    },
    {
      id: 'deploy-pull-request-previews',
      title: 'Pull request previews',
      text: '`rekcod app webhook` prints a GitHub webhook URL and secret for an app. Each pull request deploys as `blog-pr-N`, sleeps after 15 minutes without a request and is destroyed when the PR closes.',
      show: [readme.snippet('rekcod app webhook blog --remote mini')],
    },
    {
      id: 'run-s3-compatible-storage',
      title: 'S3 storage on your Mac',
      text: '`rekcod storage up` runs RustFS in the VM and prints its endpoint and credentials. A deployed app gets its own bucket and scoped keys with `bucket = true` under `[storage]`.',
      show: [readme.snippet('rekcod storage up')],
    },
    {
      id: 'manage-it-from-the-terminal-menu-bar-or-dashboard',
      title: 'Terminal, menu bar or dashboard',
      text: '`rekcod menubar` and `rekcod dashboard` offer the same actions as the CLI and run its command lines. `rekcod state` prints everything they show, with each item\'s actions, as JSON.',
      show: [readme.snippet('rekcod menubar --install')],
    },
    {
      id: 'when-something-breaks',
      title: 'One command to diagnose',
      text: '`rekcod doctor` prints a line per check and a fix for each one that fails. `--bundle` packs configuration, status and logs into a tarball, with tokens and keys left out.',
      show: [readme.snippet('rekcod doctor --bundle')],
    },
  ];

  return {
    name: 'rekcod',
    version: release.version,
    released: release.date,
    title: 'rekcod, an open source Docker Desktop alternative for Apple Silicon',
    description: metaDescription(readme.intro),
    h1: 'Docker on your Mac, without Docker Desktop',
    lede: 'rekcod is an open source Docker Desktop alternative for Apple Silicon Macs. It runs upstream [Docker Engine](https://docs.docker.com/engine/) in a Linux VM on Apple\'s `Virtualization.framework`, and your `docker` commands and Compose files work as they are. Built by [linesofcode](https://x.com/linesofcode).',
    install,
    agentCommand: `curl -fsSL ${url('/llms.txt')}`,
    split: {
      title: 'Review an agent\'s changes',
      href: '/docs/#review-an-agents-changes-before-applying-them',
      terminal: sandbox,
    },
    figures,
    measured: {
      id: 'benchmarks',
      title: 'Measured on an M5 Pro',
      text: readme.paragraph('The bench/ scripts time'),
      terminal: bench.start,
    },
    ecosystem: {
      title: 'Built for Apple Silicon Macs',
      text: 'macOS on Apple Silicon with Linux guests and Rosetta for amd64. Docker Engine and Compose, toolchains for Bun, Node.js, Python and Rust, Tailscale and Cloudflare Tunnel for servers, GitHub for runners and previews, and MCP for agents.',
      brands: [
        { name: 'apple', label: 'macOS on Apple Silicon' },
        { name: 'linux', label: 'Linux guest' },
        { name: 'docker', label: 'Docker Engine and Compose' },
        { name: 'homebrew', label: 'Homebrew' },
        { name: 'bun', label: 'Bun' },
        { name: 'nodedotjs', label: 'Node.js' },
        { name: 'python', label: 'Python' },
        { name: 'uv', label: 'uv' },
        { name: 'rust', label: 'Rust' },
        { name: 'tailscale', label: 'Tailscale' },
        { name: 'cloudflare', label: 'Cloudflare Tunnel' },
        { name: 'githubactions', label: 'GitHub Actions' },
        { name: 'modelcontextprotocol', label: 'Model Context Protocol' },
      ] satisfies Brand[],
    },
    principles: [
      { title: 'Upstream engine', text: 'rekcod runs unmodified `dockerd`, containerd and runc. Docker API behaviour comes from Docker.' },
      { title: 'One action catalog', text: 'The menu bar and dashboard run the `rekcod` command lines from one list, and parity tests fail on a gap.' },
      { title: 'Checked installs', text: 'The installer refuses a tarball that doesn\'t match `SHA256SUMS`, and checks build provenance when `gh` is there.' },
    ] satisfies Principle[],
    showcase: {
      title: 'One binary for all of it',
      text: 'The CLI, the VM daemon and the menu bar are modes of one native executable. Each tab is a block from the README; the docs have the options, defaults and failure cases for every command.',
      supports: ['docker', 'apple', 'tailscale', 'cloudflare', 'githubactions'] satisfies BrandName[],
      tabs: [
        { label: 'Snapshots', snippet: readme.snippet('rekcod snap ls', 'snapshots') },
        { label: 'Sandbox', snippet: readme.snippet('sandbox_id=$(rekcod sandbox create)', 'sandbox') },
        { label: 'Fleet', snippet: readme.snippet('rekcod node approve NODE_ID --remote mini', 'nodes') },
        { label: 'Jobs', snippet: readme.snippet('rekcod job run --remote mini -- bun test', 'jobs') },
      ] satisfies Tab[],
    },
    capabilities,
    start: {
      id: 'run-your-first-container',
      title: 'Install it',
      text: 'The installer puts `rekcod` in `/usr/local/bin` and installs nothing unless the tarball matches the release\'s `SHA256SUMS`. To check a download yourself, verify the checksum and the build attestation.',
      panels: [readme.snippet('rekcod run --rm hello-world', 'install'), readme.snippet('shasum -a 256 -c SHA256SUMS --ignore-missing', 'verify')],
    },
    today: { title: 'Run your first container today' },
    guides: [
      { title: 'Self-host apps on a Mac', text: 'A `rekcod.toml`, a push and a deploy.', href: '/docs/#deploy-an-app', snippet: readme.snippet('idle_stop = "15m"') },
      { title: 'Run a GitHub Actions runner', text: 'An arm64 Linux runner in the local VM.', href: '/docs/#run-a-self-hosted-github-actions-runner', snippet: readme.snippet('cd examples/github-runner') },
      { title: 'Give people access', text: 'A token per person, namespaces and roles.', href: '/docs/#give-people-access', snippet: readme.snippet('rekcod controller tokens --remote mini') },
    ] satisfies Guide[],
    agents: {
      minimal: readme.snippet('rekcod run --rm hello-world'),
      commands: readme.table('Command', 'What it does'),
      mistakes: [
        'A shell that sets `DOCKER_HOST` overrides Docker contexts, so `docker` talks to another engine. Commands passed to `rekcod` itself always use its local engine.',
        '`rekcod stop` removes containers created with `--rm`. Check `docker ps` before stopping or restarting a VM that runs someone else\'s containers.',
        '`rekcod sandbox apply --reviewed REVIEW` refuses once another diff of the sandbox replaced the frozen copy, or the project changed since the review. Run `sandbox diff` again and apply the new review ID.',
      ],
    },
    credit: 'rekcod is built and maintained by [linesofcode](https://linesofcode.dev) and released under the MIT license.',
  };
}

export type Landing = ReturnType<typeof buildLanding>;

/** The README intro as plain text, cut at a sentence so it stays within 160 characters. */
export function metaDescription(markdown: string): string {
  const text = markdown.replace(/[`*_]/g, '').replace(/\[([^\]]*)\]\([^)]*\)/g, '$1').replace(/\s+/g, ' ').trim();
  let out = '';
  for (const sentence of text.match(/[^.]+\./g) ?? [text]) {
    if ((out + sentence).trim().length > 160) break;
    out += sentence;
  }
  return out.trim() || text.slice(0, 157) + '...';
}

let cached: { readme: Readme; landing: Landing } | undefined;
export function load() {
  if (!cached) {
    const readme = new Readme(readRepo('README.md'));
    cached = { readme, landing: buildLanding(readme, readRepo('CHANGELOG.md')) };
  }
  return cached;
}

export type { Terminal, Snippet };
