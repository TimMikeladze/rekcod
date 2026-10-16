// The shape of `rekcod state` (crates/rekcod/src/state.rs).

export type Action = {
  kind: string
  label: string
  argv?: string[]
  url?: string
  dir?: string
  interactive: boolean
  confirm: string | null
}

export type Container = {
  id: string
  name: string
  image: string
  state: string
  status: string
  project: string | null
  /** `com.docker.compose.service`. */
  service: string | null
  ports: number[]
  /** Named volumes it mounts. */
  volumes: string[]
  actions: Action[]
}

export type Port = {
  port: number
  udp: boolean
  container: string
  error: string | null
  sandbox: boolean
  actions: Action[]
}

export type Snapshot = {
  name: string
  created: number
  volumes: string[]
  /** Bytes in its volumes, from the last `/system/df`; null when not measured. */
  size: number | null
  /** Its name is the project's current git branch: what `snap restore` picks. */
  current: boolean
  actions: Action[]
}

export type Project = {
  name: string
  dir: string | null
  containers: number
  running: number
  branch: string | null
  snapshots: Snapshot[]
  actions: Action[]
}

export type Sandbox = {
  id: string
  name: string | null
  project: string
  copy: boolean
  running: boolean
  macos: boolean
  created: number
  cpus: number
  memory_gib: number
  /** The VM process, while it runs. */
  cpu_pct: number | null
  rss_mib: number | null
  actions: Action[]
}

export type Image = {
  id: string
  tags: string[]
  size: number
  created: number
  containers: string[]
  dangling: boolean
  actions: Action[]
}

export type Volume = {
  name: string
  driver: string
  size: number | null
  containers: string[]
  anonymous: boolean
  snapshot: boolean
  actions: Action[]
}

/** `/system/df` totals in bytes, measured at `checked` (cached for two minutes). */
export type Disk = {
  checked: number
  images: number
  containers: number
  volumes: number
  build_cache: number
  build_cache_free: number
}

export type Release = {
  n: number
  commit: string
  state: string
  created: number
  who?: string
  finished?: number | null
}

export type Instance = {
  release: number
  sandbox: string
  node: string
  endpoint: string
  state: string
  /** When its sandbox was created; 0 from older controllers. */
  created?: number
}

/** One minute of edge traffic for an app. */
export type TrafficMinute = { minute: number; requests: number; errors: number; p50_ms: number; p95_ms: number }

export type AppDetail = {
  git: { url: string; ref: string }
  port: number
  instances_wanted: number
  idle_stop_secs: number
  recycle_secs: number
  last_request: number
  env: Record<string, string> | null
  secrets: string[]
  releases: Release[]
  instances: Instance[]
  traffic?: TrafficMinute[]
}

export type App = {
  name: string
  url: string
  live_release: number | null
  public: boolean
  sleeping: boolean
  detail?: AppDetail | null
  actions: Action[]
}

export type Job = {
  id: string
  name: string | null
  state: string
  exit: number | null
  created: number
  // Only with a remote (the controller's full row).
  cmd?: string[] | string | null
  node?: string
  owner?: string
  finished?: number | null
  schedule?: string | null
  ns?: string
  actions: Action[]
}

export type Node = {
  id: string
  state: string
  last_seen: number
  // The rest comes from the controller's `/nodes` (admin token).
  url?: string
  mem_total_mib?: number
  mem_free_mib?: number
  pool?: string[]
  cpus?: number
  /** 1-minute load average. */
  load?: number
  disk_total_gib?: number
  disk_free_gib?: number
  instances?: number
  jobs_running?: number
  actions: Action[]
}

export type Schedule = {
  name: string
  owner: string
  every_secs: number
  last_run: number
  last_job: string | null
  next_run?: number
}

type Quota = { sandboxes: number | "unlimited"; cpus: number | "unlimited"; memory_gib: number | "unlimited"; apps: number | "unlimited" }

export type NamespaceDetail = {
  name: string
  role: string | null
  quota: Quota
  used: { sandboxes: number; cpus: number; memory_gib: number; apps: number }
  members: { who: string; role: string }[]
}

export type Route = { app: string; ns: string; public: boolean; endpoints: string[]; sleeping: boolean; http?: { rate?: string } }

export type Ban = { net: string; reason: string; until: number | null; created: number; by?: string }
export type EdgeClient = { ip: string; requests: number; limited: number }

export type VmStatus = {
  running: boolean
  pid?: number
  dockerd?: string
  api?: string
  kernel?: string
  cpus?: number
  memory_mib?: number
  fs_cache?: boolean
  socket?: string
  context?: string | null
  default_socket?: string
  project?: string
  ports?: unknown[]
  disk?: { used_in_vm: number | null; size: number | null; on_mac: number }
  warming?: unknown
  /** virtio-fs request counters from the guest, by operation. */
  fs_requests?: Record<string, number> | null
}

export type State = {
  generated: number
  /** `rekcod state --local`: the fleet keys below are missing or stale. */
  local?: boolean
  running: boolean
  status: VmStatus
  sandbox_vm: VmStatus | null
  socket_installed: boolean
  vm_actions: Action[]
  storage_actions: Action[]
  containers: Container[]
  ports: Port[]
  projects: Project[]
  sandboxes: Sandbox[]
  images: Image[]
  volumes: Volume[]
  disk: Disk | null
  disk_actions: Action[]
  apps: App[]
  jobs: Job[]
  namespaces: { current: string; all: string[]; detail?: NamespaceDetail[]; actions: Action[] }
  hosts: { all: { name: string; endpoint: string; current: boolean }[]; local_current: boolean; actions: Action[] }
  bans: { nets: string[]; clients?: EdgeClient[]; detail?: Ban[]; actions: Action[] } | null
  remote: {
    name: string | null
    url: string
    ns: string
    all: string[]
    admin?: boolean
    /** The first failed controller call's error; the header shows it. */
    error?: string | null
    /** Each failed call's error, by what it was for (apps, jobs, nodes, ...). */
    errors?: Record<string, string>
  } | null
  remotes?: { default: string | null; all: string[]; actions: Action[] }
  controller_actions?: Action[]
  nodes: Node[] | null
  schedules: Schedule[]
  routes?: Record<string, Route> | null
  backups?: { list?: { key: string; size: number; modified: string }[]; error?: string } | null
}

export type Stat = { name: string; cpu: number; mem: number; memLimit: number; netIn: number; netOut: number }

/** One container's CPU and memory at time `t` (ms since the epoch). */
export type Sample = { t: number; name: string; cpu: number; mem: number }

/** `GET /api/stats`: the latest sample per container, the history, and each name's colour. */
export type StatsResponse = { latest: Stat[]; history: Sample[]; colors: Record<string, string>; window: number }

/** One action the page ran (`GET /api/activity`). */
export type ActivityEntry = {
  id: number
  at: number
  kind: string
  label: string
  subject: string
  argv: string[]
  code: number
  ms: number
  stdout: string
  stderr: string
  seen: boolean
  terminal?: boolean
}
