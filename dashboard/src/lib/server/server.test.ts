import { describe, expect, it } from "vitest"
import type { State } from "@/lib/types"
import { mergeLocal, staleBinary, staleMessage } from "./merge"
import { Colors, OTHER, PALETTE, parseStatLine, Ring, splitLines } from "./stats"
import { inspectArgv, instanceLogsArgv, jobHistoryArgv, logsArgv, releaseArgv, sandboxStatArgv, screenshotArgv } from "./validate"

const LINE = '{"BlockIO":"0B / 0B","CPUPerc":"1.25%","Container":"abc","ID":"abc","MemPerc":"0.5%","MemUsage":"123.5MiB / 7.753GiB","Name":"web","NetIO":"1.2kB / 3kB","PIDs":"4"}'

describe("stats stream", () => {
  it("reads a JSON line inside the redraw codes", () => {
    const s = parseStatLine(`\x1b[H\x1b[K${LINE}`)
    expect(s).toMatchObject({ name: "web", cpu: 1.25, mem: 123.5 * 1024 ** 2, netIn: 1200, netOut: 3000 })
    expect(parseStatLine("\x1b[J")).toBeNull()
    expect(parseStatLine("\x1b[H\x1b[K")).toBeNull()
    expect(parseStatLine("{not json")).toBeNull()
  })
  it("keeps a partial line for the next chunk", () => {
    const [lines, rest] = splitLines(`${LINE}\n${LINE.slice(0, 20)}`)
    expect(lines).toHaveLength(1)
    expect(rest).toBe(LINE.slice(0, 20))
  })
})

describe("Ring", () => {
  it("drops the oldest ticks and filters by window", () => {
    const r = new Ring(3)
    for (const t of [1000, 2000, 3000, 4000]) r.push([{ t, name: "a", cpu: 1, mem: 1 }])
    expect(r.ticks).toHaveLength(3)
    expect(r.window(2, 4000).map((s) => s.t)).toEqual([3000, 4000])
  })
})

describe("Colors", () => {
  it("never moves a name once assigned, whatever arrives later", () => {
    const c = new Colors()
    c.assign(["web", "db"])
    const first = c.map()
    expect(first.db).toBe(PALETTE[0])
    expect(first.web).toBe(PALETTE[1])
    c.assign(["api", "web", "db", "other"])
    const later = c.map()
    expect(later.web).toBe(first.web)
    expect(later.db).toBe(first.db)
    expect(later.api).toBe(PALETTE[2])
    expect(later.other).toBe(OTHER)
  })
})

const base = {
  generated: 1,
  running: true,
  containers: [{ name: "web" }],
  apps: [{ name: "dash", detail: { instances: [{ sandbox: "sb-1" }] } }],
  jobs: [{ name: "etl", schedule: null }],
  schedules: [{ name: "nightly" }],
  sandboxes: [
    { id: "cp1", copy: true, macos: false, running: true },
    { id: "mac1", copy: false, macos: true, running: true },
  ],
} as unknown as State

describe("mergeLocal", () => {
  it("takes docker data from --local and fleet data from the last full state", () => {
    const full = { ...base, apps: [{ name: "dash" }], remote: { url: "x" }, nodes: [{ id: "n" }], containers: [{ name: "old" }] } as unknown as State
    const local = { ...base, local: true, apps: [], remote: null, nodes: null, containers: [{ name: "new" }] } as unknown as State
    const m = mergeLocal(local, full) as unknown as Record<string, unknown>
    expect(m.containers).toEqual([{ name: "new" }])
    expect(m.apps).toEqual([{ name: "dash" }])
    expect(m.remote).toEqual({ url: "x" })
    expect(m.local).toBeUndefined()
    expect(mergeLocal(local, null).local).toBe(true)
  })
})

describe("stale binary", () => {
  it("recognises docker's answer to an unknown subcommand", () => {
    expect(staleBinary("docker: unknown command: docker state")).toBe(true)
    expect(staleBinary("docker: 'state' is not a docker command.")).toBe(true)
    expect(staleBinary("rekcod: VM not running")).toBe(false)
    expect(staleMessage("/usr/local/bin/rekcod")).toBe(
      "`/usr/local/bin/rekcod` has no `rekcod state` command; it's older than this dashboard. Set REKCOD_BIN or put a newer rekcod on PATH.",
    )
  })
})

describe("read-only routes", () => {
  const q = (o: Record<string, string>) => new URLSearchParams(o)
  it("only name things in the state", () => {
    expect(inspectArgv(base, "web")).toEqual({ argv: ["inspect", "web"] })
    expect(inspectArgv(base, "--privileged")).toHaveProperty("error")
    expect(releaseArgv(base, "dash", "3")).toEqual({ argv: ["app", "release", "dash", "3"] })
    expect(releaseArgv(base, "dash", "3; rm")).toHaveProperty("error")
    expect(releaseArgv(base, "other", "1")).toHaveProperty("error")
    expect(instanceLogsArgv(base, "dash", "sb-1")).toEqual({ argv: ["app", "logs", "dash", "--instance", "sb-1"] })
    expect(instanceLogsArgv(base, "dash", "sb-2")).toHaveProperty("error")
    expect(jobHistoryArgv(base, "nightly")).toEqual({ argv: ["job", "ls", "--name", "nightly"] })
    expect(jobHistoryArgv(base, "nope")).toHaveProperty("error")
    expect(sandboxStatArgv(base, "cp1")).toEqual({ argv: ["sandbox", "diff", "--stat", "cp1"] })
    expect(sandboxStatArgv(base, "mac1")).toHaveProperty("error")
    expect(screenshotArgv(base, "mac1", "/tmp/x.png")).toEqual({ argv: ["sandbox", "screenshot", "mac1", "/tmp/x.png"] })
    expect(screenshotArgv(base, "cp1", "/tmp/x.png")).toHaveProperty("error")
  })
  it("builds log argv from the whitelist only", () => {
    expect(logsArgv(base, q({ name: "web", follow: "1", timestamps: "1", since: "10m" }))).toEqual({
      argv: ["logs", "--tail", "1000", "--follow", "--timestamps", "--since", "10m", "web"],
    })
    expect(logsArgv(base, q({ name: "web", since: "--details" }))).toHaveProperty("error")
    expect(logsArgv(base, q({ name: "web", follow: "yes" }))).toEqual({ argv: ["logs", "--tail", "1000", "web"] })
    expect(logsArgv(base, q({ name: "db" }))).toHaveProperty("error")
  })
})
