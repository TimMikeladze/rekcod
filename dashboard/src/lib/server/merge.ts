import type { State } from "@/lib/types"

/** Keys `rekcod state --local` doesn't fill in; they come from the last full state. */
export const FLEET_KEYS = [
  "apps",
  "jobs",
  "namespaces",
  "bans",
  "remote",
  "remotes",
  "controller_actions",
  "nodes",
  "schedules",
  "routes",
  "backups",
] as const

/**
 * A `--local` state with the fleet parts of the last full one. Its `local` flag is dropped
 * once there's a full state to merge from, so the page sees one complete document.
 */
export function mergeLocal(local: State, full: State | null): State {
  if (!full) return local
  const out = { ...local } as Record<string, unknown>
  const f = full as unknown as Record<string, unknown>
  for (const k of FLEET_KEYS) if (k in f) out[k] = f[k]
  delete out.local
  return out as unknown as State
}

/** A host's engine state with this Mac's fleet parts and docker hosts. */
export function mergeEngine(remote: State, local: State | null, name: string): State {
  const out = { ...remote } as Record<string, unknown>
  const l = (local ?? {}) as Record<string, unknown>
  for (const k of [...FLEET_KEYS, "hosts"]) if (k in l) out[k] = l[k]
  delete out.local
  out.engine = name
  return out as unknown as State
}

/** What the page shows while a host's engine can't be read: this Mac's fleet, no engine parts. */
export function unreachableEngine(local: State, name: string, error: string): State {
  return {
    ...local,
    engine: name,
    engine_error: error,
    running: false,
    status: { running: false },
    sandbox_vm: null,
    vm_actions: [],
    storage_actions: [],
    containers: [],
    ports: [],
    projects: [],
    sandboxes: [],
    images: [],
    volumes: [],
    networks: [],
    engine_actions: [],
    disk: null,
    disk_actions: [],
    local_errors: {},
  }
}

/** stderr of `rekcod state` from a binary that predates it (it went to docker instead). */
export function staleBinary(stderr: string): boolean {
  return /unknown command|is not a docker command|unrecognized subcommand/i.test(stderr)
}

export function staleMessage(bin: string): string {
  return `\`${bin}\` has no \`rekcod state\` command; it's older than this dashboard. Set REKCOD_BIN or put a newer rekcod on PATH.`
}
