import { describe, expect, it } from "vitest"
import { findOffered } from "./allow"
import { duration, parseBytes } from "./format"
import { breakGaps, fillMinutes } from "./series"
import { issues } from "./issues"
import { DEFAULT_SETTINGS, fresh, notices } from "./notify"
import { filterActivity } from "@/components/activity"
import { commonKinds } from "@/components/bulk"
import { compileSearch, layout, lineLevel, windowOf } from "@/components/log-sheet"
import { entries, entryId, remember, search } from "@/components/palette"
import { errorRate } from "@/components/tabs/apps"
import { parseSort, sortRows } from "@/hooks/use-sort"
import { share } from "./share"
import type { State } from "./types"

const stop = { kind: "ContainerStop", label: "Stop", argv: ["stop", "web"], interactive: false, confirm: null }
const open = { kind: "OpenPort", label: "Open", url: "http://localhost:3000", interactive: false, confirm: null }

function state(over: Partial<State> = {}): State {
  return {
    generated: 1_000_000,
    running: true,
    status: { running: true, disk: { used_in_vm: 1, size: 100, on_mac: 1 } },
    sandbox_vm: null,
    socket_installed: true,
    vm_actions: [],
    storage_actions: [],
    containers: [{ id: "a", name: "web", image: "nginx", state: "running", status: "Up", project: null, service: null, ports: [3000], volumes: [], actions: [stop] }],
    ports: [{ port: 3000, udp: false, container: "web", error: null, sandbox: false, actions: [open] }],
    projects: [],
    sandboxes: [],
    images: [],
    volumes: [],
    disk: null,
    disk_actions: [],
    apps: [],
    jobs: [],
    namespaces: { current: "default", all: [], actions: [] },
    hosts: { all: [], local_current: true, actions: [] },
    bans: null,
    remote: null,
    nodes: null,
    schedules: [],
    ...over,
  }
}

describe("findOffered", () => {
  it("finds an action nested anywhere in the state", () => {
    expect(findOffered(state(), ["stop", "web"])).toEqual(stop)
  })
  it("refuses argv that isn't offered, partial matches and non-arrays", () => {
    expect(findOffered(state(), ["rm", "-f", "web"])).toBeNull()
    expect(findOffered(state(), ["stop"])).toBeNull()
    expect(findOffered(state(), ["stop", "web", "--extra"])).toBeNull()
    expect(findOffered(state(), "stop web")).toBeNull()
    expect(findOffered(state(), [1, 2])).toBeNull()
  })
  it("never matches url-only actions", () => {
    expect(findOffered(state(), undefined)).toBeNull()
  })
})

describe("parseBytes", () => {
  it("reads docker stats units", () => {
    expect(parseBytes("123.5MiB")).toBe(123.5 * 1024 ** 2)
    expect(parseBytes("7.753GiB ")).toBeCloseTo(7.753 * 1024 ** 3)
    expect(parseBytes("159MB")).toBe(159e6)
    expect(parseBytes("0B")).toBe(0)
    expect(parseBytes("junk")).toBeNull()
    expect(parseBytes("--")).toBeNull()
  })
})

describe("issues", () => {
  it("is empty when everything is fine", () => {
    expect(issues(state())).toEqual([])
  })
  it("ranks broken things first", () => {
    const s = state({
      ports: [{ port: 80, udp: false, container: "x", error: "port in use", sandbox: false, actions: [] }],
      status: { running: true, disk: { used_in_vm: 90, size: 100, on_mac: 1 } },
      nodes: [{ id: "studio", url: "", mem_total_mib: 100, mem_free_mib: 50, last_seen: 1_000_000 - 600, state: "ready", pool: [], actions: [] }],
      jobs: [{ id: "j1", name: null, state: "failed", exit: 1, created: 1_000_000 - 60, actions: [] }],
    })
    const list = issues(s)
    expect(list.map((i) => i.tone)).toEqual(["bad", "bad", "bad", "warn"])
    expect(list.at(-1)!.text).toBe("VM disk is 90% full")
    expect(list.some((i) => i.text.startsWith("Node studio last checked in 10m ago"))).toBe(true)
  })
  it("reports failed docker calls and doesn't call unknown free memory low", () => {
    const s = state({
      local_errors: { containers: "connection refused" },
      nodes: [{ id: "studio", url: "", mem_total_mib: 100, mem_free_mib: null, last_seen: 1_000_000, state: "ready", pool: [], actions: [] }],
    })
    expect(issues(s).map((i) => i.text)).toEqual(["Couldn't list containers: connection refused"])
  })
})

describe("duration", () => {
  it("keeps the two largest units", () => {
    expect(duration(310)).toBe("5m 10s")
    expect(duration(86_400)).toBe("1d")
    expect(duration(90_061)).toBe("1d 1h")
    expect(duration(0)).toBe("off")
  })
})

describe("remote errors", () => {
  it("are issues, not empty lists", () => {
    const s = state({ remote: { name: "studio", url: "http://x", ns: "default", all: [], error: "connection refused", errors: { apps: "connection refused" } } })
    const list = issues(s)
    expect(list[0]).toMatchObject({ tone: "bad", tab: "apps" })
    expect(list[0].text).toContain("connection refused")
  })
})

describe("notifications", () => {
  const exited = { id: "c1", name: "worker", image: "alpine", state: "exited", status: "Exited (2) 1 minute ago", project: null, service: null, ports: [], volumes: [], actions: [] }
  it("announces each event once and skips what was there on load", () => {
    const seen = new Set<string>()
    const on = { ...DEFAULT_SETTINGS, on: true }
    expect(fresh(state({ containers: [exited] }), seen, { ...on, on: false })).toEqual([])
    expect(fresh(state({ containers: [exited] }), seen, on)).toEqual([])
    const job = { id: "j2", name: "etl", state: "failed", exit: 1, created: 1, actions: [] }
    const once = fresh(state({ containers: [exited], jobs: [job] }), seen, on)
    expect(once.map((n) => n.key)).toEqual(["job:j2"])
    expect(fresh(state({ containers: [exited], jobs: [job] }), seen, on)).toEqual([])
  })
  it("leaves out disabled kinds and zero exits", () => {
    const ok = { ...exited, id: "c2", status: "Exited (0) now" }
    const off = { on: true, kinds: { ...DEFAULT_SETTINGS.kinds, container: false } }
    expect(fresh(state({ containers: [exited, ok] }), new Set(), off)).toEqual([])
    expect(notices(state({ containers: [ok] }))).toEqual([])
  })
})

describe("palette", () => {
  it("finds actions by verb and subject", () => {
    const list = entries(state())
    expect(search(list, "stop web")[0]).toMatchObject({ type: "action", text: "Stop web" })
    expect(search(list, "web").some((e) => e.type === "go" && e.href === "/containers/web")).toBe(true)
    expect(search(list, "nothing-like-this")).toEqual([])
  })
})

describe("palette pages and recents", () => {
  it("lists pages for an empty query and puts recent picks first", () => {
    const list = entries(state())
    expect(search(list, "").map((e) => e.text)).toContain("Disk")
    const recent = [entryId(search(list, "stop web")[0])]
    expect(search(list, "", recent)[0]).toMatchObject({ type: "action", text: "Stop web" })
    expect(search(list, "web", recent)[0]).toMatchObject({ type: "action", text: "Stop web" })
    expect(remember(["a", "b"], "b")).toEqual(["b", "a"])
  })
})

describe("share", () => {
  it("keeps unchanged parts' references and replaces changed ones", () => {
    const old = state()
    const next = structuredClone(old)
    next.containers.push({ ...next.containers[0], id: "b", name: "api" })
    const s = share(old, next)
    expect(s).not.toBe(old)
    expect(s.containers[0]).toBe(old.containers[0])
    expect(s.ports).toBe(old.ports)
    expect(s.containers[1].name).toBe("api")
    expect(share(old, structuredClone(old))).toBe(old)
  })
})

describe("sorting", () => {
  it("reads ?sort= and sorts with nulls last", () => {
    expect(parseSort("-mem", ["mem"])).toEqual({ key: "mem", desc: true })
    expect(parseSort("nope", ["mem"])).toBeNull()
    const rows = [{ v: 2 }, { v: null }, { v: 10 }]
    expect(sortRows(rows, (r) => r.v, false).map((r) => r.v)).toEqual([2, 10, null])
    expect(sortRows(rows, (r) => r.v, true).map((r) => r.v)).toEqual([10, 2, null])
    expect(sortRows([{ n: "web-10" }, { n: "web-9" }], (r) => r.n, false).map((r) => r.n)).toEqual(["web-9", "web-10"])
  })
})

describe("log viewer", () => {
  it("colours error and warn lines", () => {
    expect(lineLevel("error: upstream timed out")).toBe("error")
    expect(lineLevel('level=error msg="x"')).toBe("error")
    expect(lineLevel("warn: slow query")).toBe("warn")
    expect(lineLevel("GET /health 200")).toBeNull()
  })
  it("searches literally or as a regex, and reports a bad regex", () => {
    expect((compileSearch("a.b", false) as RegExp).test("axb")).toBe(false)
    expect((compileSearch("4\\d\\d", true) as RegExp).test("GET / 404")).toBe(true)
    expect(compileSearch("(", true)).toHaveProperty("error")
    expect(compileSearch("", true)).toBeNull()
  })
})

describe("bulk actions", () => {
  const rm = (n: string) => ({ kind: "ContainerRemove", label: "Remove", argv: ["rm", n], interactive: false, confirm: `Remove ${n}?` })
  const logs = (n: string) => ({ kind: "ContainerLogs", label: "Logs", argv: ["logs", n], interactive: true, confirm: null })
  it("offers only kinds every row has, never streamed or interactive ones", () => {
    const items = [
      { id: "a", subject: "a", actions: [{ ...stop, argv: ["stop", "a"] }, rm("a"), logs("a")] },
      { id: "b", subject: "b", actions: [rm("b"), logs("b")] },
    ]
    expect(commonKinds(items).map((a) => a.kind)).toEqual(["ContainerRemove"])
    expect(commonKinds([])).toEqual([])
  })
})

describe("activity filters", () => {
  const run = (id: number, code: number, subject: string) => ({ id, at: 0, kind: "K", label: "Restart", subject, argv: ["restart", subject], code, ms: 1, stdout: "", stderr: code ? "boom" : "", seen: true })
  it("filters by failure, subject and text in the output", () => {
    const list = [run(1, 0, "web"), run(2, 1, "web"), run(3, 1, "db")]
    expect(filterActivity(list, { failed: true, subject: "", q: "" }).map((e) => e.id)).toEqual([2, 3])
    expect(filterActivity(list, { failed: false, subject: "web", q: "" }).map((e) => e.id)).toEqual([1, 2])
    expect(filterActivity(list, { failed: false, subject: "", q: "BOOM" }).map((e) => e.id)).toEqual([2, 3])
  })
})

describe("errorRate", () => {
  it("is the 5xx share of the hour, null without traffic", () => {
    expect(errorRate([{ minute: 1, requests: 90, errors: 0, p50_ms: 1, p95_ms: 1 }, { minute: 2, requests: 10, errors: 5, p50_ms: 1, p95_ms: 1 }])).toBe(0.05)
    expect(errorRate([])).toBeNull()
    expect(errorRate(undefined)).toBeNull()
  })
})

describe("log windowing", () => {
  it("lays rows out by height and renders only those near the view", () => {
    const { tops, total } = layout(1000, (pos) => (pos === 1 ? 60 : 20))
    expect(tops[2]).toBe(80)
    expect(total).toBe(20 * 999 + 60)
    const [first, last] = windowOf(tops, total, 10_000, 500, 100)
    expect(tops[first]).toBeLessThanOrEqual(10_000 - 100)
    expect(tops[last - 1]).toBeGreaterThanOrEqual(10_000 + 500 - 20)
    expect(last - first).toBeLessThan(50)
    expect(windowOf(new Float64Array(0), 0, 0, 500)).toEqual([0, 0])
  })
})

describe("series", () => {
  it("fills unreported minutes with zero traffic and no latency", () => {
    const r = (minute: number) => ({ minute, requests: 50, errors: 1, p50_ms: 4, p95_ms: 9 })
    const out = fillMinutes([r(103), r(100)])
    expect(out.map((m) => m.minute)).toEqual([100, 101, 102, 103])
    expect(out[1]).toEqual({ minute: 101, requests: 0, errors: 0, p50_ms: null, p95_ms: null })
  })
  it("breaks a series where samples are further apart than the limit", () => {
    const rows = [0, 5, 30, 35].map((t) => ({ t, name: "a", v: 1 }))
    const out = breakGaps(rows, 10, (p) => ({ ...p, t: p.t + 1, v: NaN }), (r) => r.name)
    expect(out.map((r) => r.t)).toEqual([0, 5, 6, 30, 35])
    expect(out[2].v).toBeNaN()
  })
})
