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

/** stderr of `rekcod state` from a binary that predates it (it went to docker instead). */
export function staleBinary(stderr: string): boolean {
  return /unknown command|is not a docker command|unrecognized subcommand/i.test(stderr)
}

export function staleMessage(bin: string): string {
  return `\`${bin}\` has no \`rekcod state\` command; it's older than this dashboard. Set REKCOD_BIN or put a newer rekcod on PATH.`
}
