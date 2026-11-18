import "server-only"
import { spawn } from "node:child_process"

export type Ran = { code: number; stdout: string; stderr: string }

const MAX_OUTPUT = 64 << 20

/**
 * `bin argv` in a process group of its own, output captured. On `timeout` or `signal` the whole
 * group gets SIGTERM, so a child it started (the `ssh` under `rekcod host state`) goes with it;
 * execFile's timeout signals `bin` alone and leaves that child running.
 */
export function runGroup(bin: string, argv: string[], opts: { timeout: number; signal?: AbortSignal; env?: NodeJS.ProcessEnv }): Promise<Ran> {
  return new Promise((resolve) => {
    const child = spawn(/*turbopackIgnore: true*/ bin, argv, { detached: true, stdio: ["ignore", "pipe", "pipe"], env: opts.env ?? process.env })
    const out: Buffer[] = []
    const err: Buffer[] = []
    let size = 0
    let why = ""
    const kill = (reason: string) => {
      if (why) return
      why = reason
      try {
        process.kill(-child.pid!, "SIGTERM")
      } catch {}
    }
    const timer = setTimeout(() => kill(`no answer in ${Math.round(opts.timeout / 1000)} s`), opts.timeout)
    const onAbort = () => kill("cancelled")
    opts.signal?.addEventListener("abort", onAbort, { once: true })
    if (opts.signal?.aborted) onAbort()
    const keep = (list: Buffer[]) => (b: Buffer) => {
      size += b.length
      if (size > MAX_OUTPUT) return kill(`more than ${MAX_OUTPUT >> 20} MiB of output`)
      list.push(b)
    }
    child.stdout!.on("data", keep(out))
    child.stderr!.on("data", keep(err))
    let settled = false
    const done = (code: number, note = "") => {
      if (settled) return
      settled = true
      clearTimeout(timer)
      opts.signal?.removeEventListener("abort", onAbort)
      const stderr = Buffer.concat(err).toString("utf8")
      resolve({ code, stdout: Buffer.concat(out).toString("utf8"), stderr: why ? `${stderr}${stderr ? "\n" : ""}${why}` : stderr || note })
    }
    child.once("error", (e) => done(1, e.message))
    child.once("close", (code) => done(why ? 1 : (code ?? 1)))
  })
}
