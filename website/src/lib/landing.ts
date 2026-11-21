import { readFileSync } from 'node:fs';
import path from 'node:path';
import { Readme, latestRelease, type Block, type Snippet, type Terminal } from './readme';

// The landing page model: copy, section order, which README block each section shows and
// the link table. No markup. Copy is written here; every example is a README reference.

// The same variable astro.config.mjs reads, so canonical URLs follow a custom domain.
export const origin = (process.env.SITE_URL || 'https://rekcod.vercel.app').replace(/\/$/, '');
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
  { label: 'How it works', href: '/#how-it-works', icon: 'text', where: ['header'] },
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

/** One thing that crosses between macOS and the Linux guest. */
export interface Crossing { title: string; text: string; href: string; link: string; measure?: string }

export interface Capability {
  id: string;
  title: string;
  text: string;
  example: Snippet | Terminal;
  guide: string;
}

const root = path.resolve(process.cwd(), '..');

export function readRepo(file: string): string {
  return readFileSync(path.join(root, file), 'utf8');
}

export function buildLanding(readme: Readme, changelog: string) {
  const release = latestRelease(changelog);
  const firstContainer = readme.snippet('rekcod run --rm hello-world');
  const benchmark = readme.terminal('rekcod start --no-switch');
  const benchBlock: Block = benchmark.block;
  const figures: Figure[] = [
    { value: `${readme.figure(benchBlock, /rekcod ready in ([\d.]+)s/)} s`, label: 'Stopped VM to Docker ready' },
    { value: `${readme.figure(benchBlock, /inotify round trip .*?p50 ([\d.]+) ms/)} ms`, label: 'File change round trip, median' },
    { value: `${readme.figure(benchBlock, /mac p50\s+([\d.]+) µs/)} µs`, label: 'Published TCP port round trip, median' },
    { value: Number(readme.figure(benchBlock, /(\d+) req\/s/)).toLocaleString('en-US'), label: 'TCP requests per second across 8 connections' },
  ];

  const capabilities: Capability[] = [
    {
      id: 'run-the-project-you-already-have',
      title: 'Use your existing Compose file',
      text: 'Run `rekcod compose up -d` from your project directory. You can also use `docker compose` while the `rekcod` context is active. If you’re switching from Docker Desktop or OrbStack, [copy your images and named volumes](/docs/#bring-your-images-and-volumes) before starting your project.',
      example: readme.snippet('rekcod ps'),
      guide: 'Compose commands',
    },
    {
      id: 'keep-dependency-writes-inside-linux',
      title: 'Keep dependency writes in Linux',
      text: 'List paths such as `node_modules` and `.next` in the `dev.rekcod.local` Compose label. Installs and builds write to Linux volumes, and edits to your Mac source files still trigger inotify for file watchers in the container.',
      example: readme.snippet('dev.rekcod.local: "node_modules,.next"'),
      guide: 'Linux volume configuration',
    },
    {
      id: 'restore-your-database-to-a-known-state',
      title: 'Reset your local database',
      text: 'After migrating and seeding, run `rekcod snap save` to copy the Compose project’s named volumes. Use `rekcod snap restore` after a test run or failed migration to return to that state. Restore saves the data it replaces as `pre-restore`.',
      example: readme.terminal('rekcod snap save seeded'),
      guide: 'Snapshot and restore options',
    },
    {
      id: 'run-project-tools-without-installing-them-on-your-mac',
      title: 'Run Linux tools from your project',
      text: '`rekcod x` runs commands such as `bun install` and `cargo test` in Linux. It picks an image using the command and version files such as `.nvmrc` or `rust-toolchain.toml`, mounts your project, and forwards listening ports to your Mac.',
      example: readme.snippet('rekcod x cargo test'),
      guide: 'Toolchain and image selection',
    },
    {
      id: 'review-an-agents-changes-before-applying-them',
      title: 'Run coding agents in a VM',
      text: '`rekcod sandbox create` copies your project into a separate VM with its own Docker engine. After the agent runs commands there, use `rekcod sandbox diff` to review the changes and `rekcod sandbox apply --reviewed` with the printed review ID to apply them.',
      example: readme.snippet('sandbox_id=$(rekcod sandbox create)'),
      guide: 'Sandbox review and apply',
    },
  ];

  const arch = readme.diagram('Local Docker architecture');
  const need = (map: Record<string, string>, key: string) => {
    if (!map[key]) throw new Error(`README.md: the architecture diagram has no ${key}`);
    return map[key];
  };
  const diagram = {
    href: '/docs/#docker-runs-inside-a-linux-vm',
    startCommand: 'rekcod start',
    composeCommand: 'docker compose up -d',
    mac: need(arch.nodes, 'mac'),
    guest: need(arch.nodes, 'guest'),
    clients: need(arch.nodes, 'clients'),
    daemon: need(arch.nodes, 'daemon'),
    agent: need(arch.nodes, 'agent'),
    engine: need(arch.nodes, 'engine'),
    containerd: need(arch.nodes, 'containerd'),
    runc: need(arch.nodes, 'runc'),
    containers: need(arch.nodes, 'containers'),
    socket: need(arch.edges, 'clients-daemon'),
    vsock: need(arch.edges, 'daemon-agent'),
    // A container name from a captured run in the README.
    container: readme.figure(readme.terminal("rekcod exec shop-web-1 sh -c 'mount | grep /app'").block, /\$ rekcod exec (\S+)/),
    share: 'virtio-fs, same path',
    silicon: 'Apple Silicon Mac',
    arch: 'arm64, M-series',
    native: 'linux/arm64 images run natively',
    rosetta: 'linux/amd64 images run through Rosetta',
    project: '~/project',
  };

  const inotify = figures[1].value;
  const tcp = figures[2].value;
  const crossings: Crossing[] = [
    {
      title: 'Docker API',
      text: 'The `rekcod` Docker context points the Docker CLI, Compose and SDKs at a Unix socket on the Mac. The daemon forwards each request over vsock to `rekcod-agent`, which connects it to `/var/run/docker.sock` in the guest.',
      href: '/docs/#docker-runs-inside-a-linux-vm',
      link: 'How a request reaches the engine',
    },
    {
      title: 'Files',
      text: 'virtio-fs shares Mac directories with the guest at their Mac paths, so a bind mount reads your source without a copy. A watcher sends macOS FSEvents to the guest, which emits inotify for file watchers in containers.',
      href: '/docs/#file-sharing-includes-a-change-notification-path',
      link: 'File sharing and change notifications',
      measure: `${inotify} median round trip`,
    },
    {
      title: 'Published ports',
      text: 'The daemon listens on the Mac and the guest agent connects incoming traffic to the container’s published port. `host.docker.internal` uses a channel in the other direction to reach services on the Mac’s loopback address.',
      href: '/docs/#published-ports',
      link: 'Which interfaces a port listens on',
      measure: `${tcp} median round trip`,
    },
    {
      title: 'CPU architecture',
      text: '`linux/arm64` images run natively on the M-series chip. `linux/amd64` images run through Rosetta, which you install on the Mac.',
      href: '/docs/#amd64-images',
      link: 'Running amd64 images',
    },
    {
      title: 'Memory',
      text: 'On macOS 27, half of `--memory` is RAM and the rest is swap whose memory goes back to macOS once the guest frees it. On older macOS all of it is RAM, held until `rekcod stop`.',
      href: '/docs/#configure-the-vm',
      link: 'CPU, memory and disk settings',
    },
  ];

  // The second half of the README's file sharing diagram: what happens after an edit on the Mac.
  const events = readme.sequence('Shared files and change notifications');
  const firstEvent = events.messages.findIndex((m) => m.text.startsWith('FSEvents'));
  if (firstEvent < 0) throw new Error('README.md: the file sharing diagram has no FSEvents message');
  const who = (id: string) => {
    if (!events.participants[id]) throw new Error(`README.md: the file sharing diagram has no participant ${id}`);
    return events.participants[id];
  };
  const steps = events.messages.slice(firstEvent).map((m) => ({ from: who(m.from), to: who(m.to), text: m.text }));

  const limits = readme.list('limitations');

  return {
    name: 'rekcod',
    why: {
      id: 'why-a-vm',
      title: 'Docker on a Mac runs in a Linux VM',
      text: 'Containers are Linux processes, and macOS has no Linux kernel to run them. Every Docker setup on a Mac boots a Linux VM, and each `docker` command, file read and published port crosses from macOS into it. rekcod is the Mac side of that crossing. It boots the VM, shares your files, forwards ports and applies the Mac’s DNS settings. The engine inside is the `dockerd` Docker ships for Linux.',
    },
    how: {
      id: 'how-it-works',
      title: 'What crosses between macOS and Linux',
      text: 'The drawing follows a `docker compose up -d` from your terminal to a running container. Beside it is how requests, files and ports cross into the guest, and how the VM uses the Mac’s CPU and memory.',
      crossings,
    },
    files: {
      id: 'file-events',
      title: 'How a Mac edit reaches a file watcher',
      text: `A shared filesystem lets a container read your source. Learning that a file changed on the Mac is a separate path. rekcod sends each macOS FSEvents change to the guest over vsock, where a patched FUSE module drops the cached entry and emits the event. An edit on the Mac, the container seeing it and writing a file back takes ${inotify} at the median.`,
      steps,
      href: '/docs/#file-sharing-includes-a-change-notification-path',
    },
    decisions: {
      id: 'design',
      title: 'Why it’s built this way',
      items: [
        {
          title: 'Upstream Docker Engine',
          text: 'rekcod runs unmodified `dockerd`, containerd and runc with the embedded BuildKit. Docker API behavior comes from Docker itself, so Compose, Buildx and Docker SDK clients talk to the engine they were written for.',
          href: '/docs/#docker-runs-inside-a-linux-vm',
        },
        {
          title: 'One native executable',
          text: 'The CLI, daemon and menu bar are modes of the same binary. The menu bar and web dashboard read one action catalog and run the `rekcod` commands it lists, so a container operation has one implementation.',
          href: '/docs/#manage-it-from-the-terminal-menu-bar-or-dashboard',
        },
        {
          title: 'Separate guest and data disks',
          text: '`vm/root.img` holds the Linux guest and is replaced when the guest version changes. `vm/data.img` holds your images, containers, named volumes and snapshots and is kept across upgrades.',
          href: '/docs/#guest-updates-and-application-data-use-separate-files',
        },
        {
          title: 'Checked releases',
          text: '`install.sh` and `rekcod upgrade` install nothing unless the tarball matches the release’s `SHA256SUMS`, and with `gh` installed they verify the build provenance too. `rekcod upgrade --rollback` puts the previous binary back.',
          href: '/docs/#what-upgrade-verifies',
        },
      ],
    },
    commands: {
      id: 'commands',
      title: 'What you run',
      text: 'Your `docker` and `docker compose` commands work as they are. These are the commands rekcod adds for local development.',
    },
    switching: {
      id: 'bring-your-images-and-volumes',
      title: 'Try it beside Docker Desktop or OrbStack',
      text: '`rekcod start` selects the `rekcod` Docker context and `rekcod stop` restores the one you had, so your current engine stays installed. `rekcod import` copies images and named volumes and leaves the originals at the source.',
      example: readme.snippet('rekcod import --from orbstack'),
      guide: 'Import options',
    },
    limitations: {
      id: 'limitations',
      title: 'Limitations',
      text: readme.paragraph("rekcod hasn't had an external security review").replace(/\]\(#/g, '](/docs/#'),
      list: limits.markdown.replace(/\]\(#/g, '](/docs/#'),
      items: limits.items,
      href: '/docs/#limitations',
    },
    diagram,
    version: release.version,
    released: release.date,
    title: 'rekcod | Docker Engine on Apple Silicon',
    description: metaDescription(readme.intro),
    h1: 'Docker Engine on Apple Silicon',
    lede: 'rekcod is an open source Docker Desktop alternative. It runs upstream Docker Engine in a Linux VM using Apple’s Virtualization framework, so you can use your existing Docker commands, Compose files and Buildx workflows.',
    meta: 'Open source · MIT license · Apple Silicon Macs',
    start: {
      id: 'run-your-first-container',
      title: 'Install and run',
      text: 'On first start, rekcod downloads the Linux guest image and selects the `rekcod` Docker context. If the Docker CLI is missing, rekcod installs it along with Compose and Buildx.',
      example: firstContainer,
      href: '/docs/#run-your-first-container',
    },
    agentCommand: `curl -fsSL ${url('/llms.txt')}`,
    figures,
    measured: {
      id: 'benchmarks',
      title: 'Benchmarks',
      text: readme.paragraph('The bench/ scripts time')
        .replace(/^The `bench\/` scripts time the paths above\. On an /, 'Measured on an ')
        .replace(/:$/, '.'),
      method: readme.paragraph('The start booted a stopped VM'),
      terminal: benchmark,
      href: '/docs/#benchmarks',
    },
    capabilities,
    guides: [
      { label: 'Configure VM CPU, memory and disk', href: '/docs/#configure-the-vm' },
      { label: 'Use another Mac as your Docker engine', href: '/docs/#use-another-mac-as-your-docker-engine' },
      { label: 'Deploy an app to a Mac', href: '/docs/#deploy-an-app' },
      { label: 'Use the menu bar or web dashboard', href: '/docs/#manage-it-from-the-terminal-menu-bar-or-dashboard' },
      { label: 'Review platform and networking limits', href: '/docs/#limitations' },
    ],
    agents: {
      sandbox: readme.terminal('rekcod sandbox diff "$id"'),
      commands: readme.table('Command', 'What it does'),
      mistakes: [
        'A shell that sets `DOCKER_HOST` overrides Docker contexts, so `docker` talks to another engine. Commands passed to `rekcod` itself always use its local engine.',
        '`rekcod stop` removes containers created with `--rm`. Check `docker ps` before stopping or restarting a VM that runs someone else\'s containers.',
        '`rekcod sandbox apply --reviewed REVIEW` refuses once another diff of the sandbox replaced the frozen copy, or the project changed since the review. Run `sandbox diff` again and apply the new review ID.',
      ],
    },
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
