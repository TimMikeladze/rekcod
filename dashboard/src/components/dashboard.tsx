"use client"

import { Suspense, useEffect, useMemo, useRef } from "react"
import Link from "next/link"
import { usePathname, useRouter, useSearchParams } from "next/navigation"
import { ChevronRight, Search } from "lucide-react"
import { ActionProvider, Actions } from "@/components/actions"
import { Meter } from "@/components/bits"
import { Palette } from "@/components/palette"
import { ResourceChart, UsageSpark } from "@/components/resource-chart"
import { SECTIONS } from "@/components/shell/nav"
import { PageTitle } from "@/components/shell/page-title"
import { DrawerProvider, Sidebar } from "@/components/shell/sidebar"
import { Shortcuts } from "@/components/shell/shortcuts"
import { TopBar } from "@/components/shell/top-bar"
import { Apps } from "@/components/tabs/apps"
import { Containers } from "@/components/tabs/containers"
import { Fleet } from "@/components/tabs/fleet"
import { Images } from "@/components/tabs/images"
import { Jobs } from "@/components/tabs/jobs"
import { Ports } from "@/components/tabs/ports"
import { Projects } from "@/components/tabs/projects"
import { Sandboxes } from "@/components/tabs/sandboxes"
import { Volumes } from "@/components/tabs/volumes"
import { Input } from "@/components/ui/input"
import { Skeleton } from "@/components/ui/skeleton"
import { DashContext, useDash } from "@/hooks/dash-context"
import { useDashboard } from "@/hooks/use-dashboard"
import { ago, fmtBytes } from "@/lib/format"
import { issues, type Tab } from "@/lib/issues"
import type { State as S, Stat } from "@/lib/types"

/**
 * Everything every page shares: live data (`useDash()`), the action runner, the sidebar, the
 * top bar, the palette. Pages render once the first state has arrived.
 */
export function Shell({ children }: { children: React.ReactNode }) {
  const dash = useDashboard()
  const { state, error, refresh } = dash
  const remoteError = state?.remote?.error
  return (
    <DashContext.Provider value={dash}>
      <ActionProvider onDone={refresh}>
        <Suspense>
        <DrawerProvider>
        <div className="flex min-h-screen flex-col md:flex-row">
          {/* Both read the path, which a dynamic route only knows in the browser. */}
          <Suspense fallback={<div className="border-r bg-sidebar md:h-screen md:w-[248px] md:flex-none rail:w-[60px]" />}>
            <Sidebar />
          </Suspense>
          <div className="flex min-w-0 flex-1 flex-col">
            <Suspense fallback={<div className="h-14 border-b" />}>
              <TopBar />
            </Suspense>
            <main className="page flex flex-col gap-6">
              {remoteError && (
                <div role="alert" className="flex flex-wrap items-baseline gap-x-2 rounded-lg border border-alert-border bg-alert-bg px-3.5 py-2.5 text-[13px]">
                  <span className="font-medium text-alert-text">Remote {state?.remote?.name ?? ""} isn&apos;t answering</span>
                  <span className="min-w-0 font-mono text-xs break-all text-muted-foreground">{remoteError}</span>
                </div>
              )}
              {!state ? (
                error ? (
                  <div role="alert" className="rounded-lg border border-alert-border bg-alert-bg p-4 text-sm">
                    <h1 className="font-medium">Can&apos;t read rekcod state</h1>
                    <pre className="mt-1 font-mono text-xs whitespace-pre-wrap text-muted-foreground">{error}</pre>
                    <p className="mt-2 text-xs text-muted-foreground">
                      The dashboard runs <code>rekcod state</code>. Set <code>REKCOD_BIN</code> if rekcod isn&apos;t on PATH.
                    </p>
                  </div>
                ) : (
                  <Loading />
                )
              ) : (
                <>
                  {children}
                  <Palette state={state} />
                </>
              )}
            </main>
          </div>
        </div>
        </DrawerProvider>
        </Suspense>
        <Shortcuts />
      </ActionProvider>
    </DashContext.Provider>
  )
}

function Loading() {
  return (
    <div className="space-y-6">
      <Skeleton className="h-9 w-48" />
      <div className="grid grid-cols-2 gap-3 md:grid-cols-3 lg:grid-cols-6">
        {Array.from({ length: 6 }, (_, i) => (
          <Skeleton key={i} className="h-20" />
        ))}
      </div>
      <Skeleton className="h-56" />
    </div>
  )
}

// ------------------------------------------------------------ overview

/** The front page: tiles, what needs attention, the charts. `/?tab=X&q=Y` (old links) goes to `/X?q=Y`. */
export function Overview() {
  return (
    <Suspense>
      <OverviewBody />
    </Suspense>
  )
}

function OverviewBody() {
  const { state, stats } = useDash()
  const params = useSearchParams()
  const router = useRouter()
  const legacy = SECTIONS.find((s) => s.tab === params.get("tab"))
  useEffect(() => {
    if (!legacy) return
    const q = params.get("q")
    router.replace(q ? `${legacy.href}?q=${encodeURIComponent(q)}` : legacy.href)
  }, [legacy, params, router])
  if (!state || legacy) return null
  return (
    <>
      <PageTitle title="Overview">What runs on this Mac and on the fleet, and what needs a look.</PageTitle>
      <Tiles state={state} stats={stats} />
      <Attention state={state} />
      <RecentActivity />
      {state.running && (
        <div className="grid gap-6 lg:grid-cols-2">
          <Panel title="CPU by container">
            <ResourceChart metric="cpu" />
          </Panel>
          <Panel title="Memory by container">
            <ResourceChart metric="mem" />
          </Panel>
        </div>
      )}
    </>
  )
}

/** The last few things this dashboard ran; the full list is `/activity`. */
function RecentActivity() {
  const { activity } = useDash()
  if (!activity.length) return null
  return (
    <section aria-labelledby="recent-h">
      <div className="mb-3 flex items-baseline justify-between">
        <h2 id="recent-h" className="h2">
          Recent activity
        </h2>
        <Link href="/activity" className="cap hover:text-foreground">
          View all →
        </Link>
      </div>
      <ol className="flex flex-col gap-2.5 text-[13px]">
        {activity.slice(0, 5).map((e) => (
          <li key={e.id} className="flex min-w-0 items-baseline gap-2.5">
            <span className="dot translate-y-[-1px]" style={{ background: e.code === 0 ? "var(--success)" : "var(--alert)" }} aria-hidden />
            <span className="min-w-0 flex-1 truncate">
              <span className="font-medium">{e.label}</span> <span className="text-muted-foreground">{e.subject}</span>{" "}
              <span className="font-mono text-xs text-muted-foreground">rekcod {e.argv.join(" ")}</span>
            </span>
            <span className={e.code === 0 ? "text-muted-foreground" : "text-alert-text"}>{e.code === 0 ? "ok" : `exit ${e.code}`}</span>
            <span className="w-16 text-right whitespace-nowrap text-muted-foreground tabular-nums">{ago(e.at / 1000)}</span>
          </li>
        ))}
      </ol>
    </section>
  )
}

function Panel({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <section aria-label={title}>
      <h2 className="h2 mb-3">{title}</h2>
      <div className="rounded-lg border bg-card p-4">{children}</div>
    </section>
  )
}

function Tile({ label, value, sub, href, children }: { label: string; value: React.ReactNode; sub?: React.ReactNode; href?: string; children?: React.ReactNode }) {
  const body = (
    <>
      <div className="cap">{label}</div>
      <div className="mt-0.5 text-lg font-semibold tabular-nums">{value}</div>
      {sub && <div className="truncate text-xs text-muted-foreground">{sub}</div>}
      {children}
    </>
  )
  const cls = "flex min-w-0 flex-col rounded-lg border bg-card p-3"
  return href ? (
    <Link href={href} className={`${cls} transition-colors hover:border-strong focus-visible:ring-2 focus-visible:ring-ring/50 focus-visible:outline-none`}>
      {body}
    </Link>
  ) : (
    <div className={cls}>{body}</div>
  )
}

function Tiles({ state, stats }: { state: S; stats: Record<string, Stat> }) {
  const running = state.containers.filter((c) => c.state === "running").length
  const cpu = Object.values(stats).reduce((a, s) => a + s.cpu, 0)
  const mem = Object.values(stats).reduce((a, s) => a + s.mem, 0)
  const memTotal = (state.status.memory_mib ?? 0) * 1024 * 1024
  const disk = state.status.disk
  const live = state.apps.filter((a) => !a.sleeping && a.live_release != null).length
  const asleep = state.apps.filter((a) => a.sleeping).length
  const jobsRunning = state.jobs.filter((j) => j.state === "running").length
  const jobsFailed = state.jobs.filter((j) => j.state === "failed" || j.state === "lost").length
  const appsErr = state.remote?.errors?.apps
  const jobsErr = state.remote?.errors?.jobs ?? appsErr
  return (
    <div className="grid grid-cols-2 gap-3 md:grid-cols-3 xl:grid-cols-6">
      <Tile label="Containers" href="/containers" value={`${running}/${state.containers.length}`} sub={`${state.containers.length - running} stopped`} />
      <Tile label="CPU" value={`${cpu.toFixed(1)}%`} sub={`of ${(state.status.cpus ?? 0) * 100}% (${state.status.cpus ?? "?"} vCPU)`}>
        {state.running && <div className="mt-1.5"><UsageSpark metric="cpu" /></div>}
      </Tile>
      <Tile label="Memory" value={fmtBytes(mem)} sub={`of ${fmtBytes(memTotal)} VM`}>
        {state.running && <div className="mt-1.5"><UsageSpark metric="mem" /></div>}
        <Meter value={memTotal ? mem / memTotal : 0} className="mt-1" />
      </Tile>
      <Tile label="Disk" href="/disk" value={fmtBytes(disk?.used_in_vm)} sub={`${fmtBytes(disk?.on_mac)} on the Mac`}>
        <Meter value={disk?.size ? (disk.used_in_vm ?? 0) / disk.size : 0} className="mt-2" />
      </Tile>
      <Tile
        label="Apps"
        href="/apps"
        value={state.remote && !appsErr ? `${live}/${state.apps.length}` : "–"}
        sub={!state.remote ? "no remote" : appsErr ? "remote error" : `${asleep} asleep · ns ${state.namespaces.current}`}
      />
      <Tile
        label="Jobs"
        href="/jobs"
        value={state.remote && !jobsErr ? String(jobsRunning) : "–"}
        sub={!state.remote ? "no remote" : jobsErr ? "remote error" : `running · ${jobsFailed} failed recently`}
      />
    </div>
  )
}

const TONE_COLOR = { bad: "var(--alert)", warn: "var(--warn)", info: "var(--ok)", ok: "var(--success)", idle: "var(--strong)" } as const

function Attention({ state }: { state: S }) {
  const list = useMemo(() => issues(state), [state])
  return (
    <section aria-labelledby="attention-h">
      <h2 id="attention-h" className="h2 mb-3">
        Needs attention{list.length > 0 && <span className="cap font-normal"> · {list.length}</span>}
      </h2>
      {!list.length ? (
        <div className="rounded-lg border border-dashed px-6 py-8 text-center">
          <div className="text-[15px] font-medium">Nothing needs attention</div>
          <p className="cap mt-1">
            Failed releases, lost instances, quiet nodes, failed jobs and containers that keep restarting show up here.
          </p>
          {!state.running && (
            <div className="mt-4 flex justify-center gap-2 text-sm">
              The VM is stopped. <Actions actions={state.vm_actions.filter((a) => a.kind === "VmStart")} subject="rekcod" inline={1} />
            </div>
          )}
        </div>
      ) : (
        <ul className="overflow-hidden rounded-lg border bg-card">
          {list.slice(0, 8).map((i) => (
            <li key={i.text} className="border-b border-hairline last:border-b-0">
              <Link href={`/${i.tab}`} className="flex items-center gap-3 px-4 py-3 text-sm hover:bg-subtle">
                <span className="dot" style={{ background: TONE_COLOR[i.tone] }} aria-hidden />
                <span className="min-w-0 flex-1">{i.text}</span>
                <span className="hidden text-xs text-muted-foreground capitalize sm:inline">{i.tab}</span>
                <ChevronRight className="size-3.5 flex-none text-muted-foreground" aria-hidden />
              </Link>
            </li>
          ))}
          {list.length > 8 && <li className="cap px-4 py-2.5">and {list.length - 8} more</li>}
        </ul>
      )}
    </section>
  )
}

// ------------------------------------------------------------ list pages

const INTRO: Record<Tab, string> = {
  containers: "Everything dockerd runs in the VM, grouped by compose project. Open a row for its logs, inspect data and chart.",
  images: "Images in the VM and the containers using them.",
  volumes: "Volumes in the VM and the containers mounting them.",
  apps: "Apps on the default remote, in the current namespace.",
  jobs: "One-off and scheduled jobs on the default remote, newest first.",
  sandboxes: "Agent sandboxes on this Mac.",
  projects: "Compose projects and their snapshots.",
  ports: "Container ports forwarded to the Mac.",
  fleet: "Nodes, namespaces, edge routes, backups, bans, remotes and docker hosts.",
}

const BODY: Record<Tab, (p: { q: string }) => React.ReactNode> = {
  containers: Containers,
  images: Images,
  volumes: Volumes,
  apps: Apps,
  jobs: Jobs,
  sandboxes: Sandboxes,
  projects: Projects,
  ports: Ports,
  fleet: Fleet,
}

/** One list page (`/containers`, `/apps`, ...): its title, the filter (`?q=`, `/` focuses it) and the rows. */
export function SectionPage({ tab }: { tab: Tab }) {
  return (
    <Suspense>
      <SectionBody tab={tab} />
    </Suspense>
  )
}

function SectionBody({ tab }: { tab: Tab }) {
  const { state } = useDash()
  const params = useSearchParams()
  const router = useRouter()
  const pathname = usePathname()
  const search = useRef<HTMLInputElement>(null)
  const q = params.get("q") ?? ""
  // Keeps the other parameters (`sort`, `view`).
  const setQ = (v: string) => {
    const p = new URLSearchParams(params.toString())
    if (v) p.set("q", v)
    else p.delete("q")
    const qs = p.toString()
    router.replace(qs ? `${pathname}?${qs}` : pathname, { scroll: false })
  }

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const typing = e.target instanceof HTMLInputElement || e.target instanceof HTMLTextAreaElement
      if (e.key === "/" && !typing) {
        e.preventDefault()
        search.current?.focus()
      }
    }
    window.addEventListener("keydown", onKey)
    return () => window.removeEventListener("keydown", onKey)
  }, [])

  const section = SECTIONS.find((s) => s.tab === tab)!
  const Body = BODY[tab]
  if (!state) return null
  return (
    <>
      <PageTitle
        title={section.label}
        actions={
          <div className="relative w-full sm:w-72">
            <Search className="absolute top-1/2 left-2.5 size-3.5 -translate-y-1/2 text-muted-foreground" aria-hidden />
            <Input
              ref={search}
              value={q}
              onChange={(e) => setQ(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Escape") {
                  setQ("")
                  e.currentTarget.blur()
                }
              }}
              placeholder="Filter"
              className="h-9 pr-8 pl-8"
              aria-label="Filter rows"
            />
            <span className="kbd absolute top-1/2 right-2 -translate-y-1/2" aria-hidden>
              /
            </span>
          </div>
        }
      >
        {INTRO[tab]}
      </PageTitle>
      <section aria-label={section.label}>
        <Body q={q.trim().toLowerCase()} />
      </section>
    </>
  )
}

