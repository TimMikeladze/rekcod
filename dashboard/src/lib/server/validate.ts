// The read-only routes build their argv here from query parameters checked against the
// current state. Nothing from the page goes into argv unless it names something in the state
// or is one of the fixed flag values below.
import type { State } from "@/lib/types"

export const SINCE = ["1m", "10m", "1h", "6h", "24h"] as const

type Argv = { argv: string[] } | { error: string }

const no = (error: string): Argv => ({ error })

export function inspectArgv(s: State, name: string | null): Argv {
  if (!name || !s.containers.some((c) => c.name === name)) return no("no such container")
  return { argv: ["inspect", name] }
}

export function logsArgv(s: State, q: URLSearchParams): Argv {
  const name = q.get("name")
  if (!name || !s.containers.some((c) => c.name === name)) return no("no such container")
  const argv = ["logs", "--tail", "1000"]
  if (q.get("follow") === "1") argv.push("--follow")
  if (q.get("timestamps") === "1") argv.push("--timestamps")
  const since = q.get("since")
  if (since) {
    if (!(SINCE as readonly string[]).includes(since)) return no(`since must be one of ${SINCE.join(", ")}`)
    argv.push("--since", since)
  }
  argv.push(name)
  return { argv }
}

export function releaseArgv(s: State, app: string | null, n: string | null): Argv {
  if (!app || !s.apps.some((a) => a.name === app)) return no("no such app")
  if (!n || !/^\d{1,9}$/.test(n)) return no("release numbers are integers")
  return { argv: ["app", "release", app, n] }
}

export function instanceLogsArgv(s: State, app: string | null, id: string | null): Argv {
  const a = s.apps.find((x) => x.name === app)
  if (!a || !app) return no("no such app")
  if (!id || !a.detail?.instances.some((i) => i.sandbox === id)) return no("no such instance")
  return { argv: ["app", "logs", app, "--instance", id] }
}

export function jobHistoryArgv(s: State, name: string | null): Argv {
  const known = name && (s.jobs.some((j) => j.name === name || j.schedule === name) || s.schedules.some((x) => x.name === name))
  if (!known || !name) return no("no such job name or schedule")
  return { argv: ["job", "ls", "--name", name] }
}

export function sandboxStatArgv(s: State, id: string | null): Argv {
  const sb = s.sandboxes.find((x) => x.id === id)
  if (!sb || !id) return no("no such sandbox")
  if (!sb.copy) return no("only copy sandboxes have a diff")
  return { argv: ["sandbox", "diff", "--stat", id] }
}

/** The tmp file is the server's; the page only picks a running macOS sandbox. */
export function screenshotArgv(s: State, id: string | null, file: string): Argv {
  const sb = s.sandboxes.find((x) => x.id === id)
  if (!sb || !id) return no("no such sandbox")
  if (!sb.macos || !sb.running) return no("screenshots are for running macOS sandboxes")
  return { argv: ["sandbox", "screenshot", id, file] }
}
