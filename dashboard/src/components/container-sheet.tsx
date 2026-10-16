"use client"

import { useEffect, useState, type ReactNode } from "react"
import { Actions } from "@/components/actions"
import { DetailHead } from "@/components/shell/page-title"
import { containerTone, Empty, State } from "@/components/bits"
import { LogView } from "@/components/log-sheet"
import { ResourceChart } from "@/components/resource-chart"
import { PinButton } from "@/components/shell/pins"
import { TabRow, useTab, type TabItem } from "@/components/shell/tab-row"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Check, Copy } from "lucide-react"
import { useDash } from "@/hooks/dash-context"
import { ago } from "@/lib/format"

/** The parts of `docker inspect` the sheet shows. */
type Inspect = {
  Created?: string
  RestartCount?: number
  Path?: string
  Args?: string[]
  State?: {
    Status?: string
    ExitCode?: number
    StartedAt?: string
    FinishedAt?: string
    Health?: { Status?: string; FailingStreak?: number; Log?: { Start?: string; End?: string; ExitCode?: number; Output?: string }[] }
  }
  Config?: { Cmd?: string[] | null; Entrypoint?: string[] | null; Env?: string[] | null; Labels?: Record<string, string> | null; WorkingDir?: string; User?: string }
  HostConfig?: { RestartPolicy?: { Name?: string } }
  Mounts?: { Type?: string; Name?: string; Source?: string; Destination?: string; RW?: boolean }[]
  NetworkSettings?: { Networks?: Record<string, { IPAddress?: string; Aliases?: string[] | null }> }
}

const secs = (iso?: string) => (iso && !iso.startsWith("0001") ? Date.parse(iso) / 1000 : 0)

function Section({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section>
      <h2 className="h2 mb-3">{title}</h2>
      {children}
    </section>
  )
}

/** One env var, its value hidden until clicked. */
function EnvVar({ entry }: { entry: string }) {
  const [shown, setShown] = useState(false)
  const i = entry.indexOf("=")
  const [k, v] = i < 0 ? [entry, ""] : [entry.slice(0, i), entry.slice(i + 1)]
  return (
    <li className="flex min-w-0 items-baseline gap-2 font-mono text-xs">
      <span className="shrink-0 text-muted-foreground">{k}</span>
      <button
        type="button"
        onClick={() => setShown((s) => !s)}
        aria-label={shown ? `Hide ${k}` : `Show ${k}`}
        className="min-w-0 truncate text-left hover:underline"
        title={shown ? "Click to hide" : "Click to show"}
      >
        {shown ? v || "(empty)" : "••••••"}
      </button>
    </li>
  )
}

const TABS: TabItem[] = [
  { value: "overview", label: "Overview" },
  { value: "logs", label: "Logs" },
  { value: "inspect", label: "Inspect" },
  { value: "env", label: "Env" },
]

/** Env vars, values hidden until clicked. */
function Env({ env, loaded }: { env?: string[] | null; loaded: boolean }) {
  if (!env?.length) return <p className="text-xs text-muted-foreground">{loaded ? "No env vars." : "loading…"}</p>
  return (
    <ul className="facts block space-y-0 px-0 py-1">
      {env.map((e) => (
        <li key={e.split("=")[0]} className="border-t border-hairline px-3.5 py-2 first:border-t-0">
          <EnvVar entry={e} />
        </li>
      ))}
    </ul>
  )
}

/** The whole `docker inspect` document, env values masked, with a copy button. */
function InspectJson({ value }: { value: Inspect | null }) {
  const [copied, setCopied] = useState(false)
  if (!value) return <p className="text-xs text-muted-foreground">loading…</p>
  const masked = { ...value, Config: value.Config && { ...value.Config, Env: value.Config.Env?.map((e) => e.replace(/=.*/, "=••••••")) } }
  const text = JSON.stringify(masked, null, 2)
  return (
    <div className="relative">
      <Button
        size="xs"
        variant="outline"
        className="absolute top-2 right-2 bg-background"
        onClick={async () => {
          try {
            await navigator.clipboard.writeText(JSON.stringify(value, null, 2))
            setCopied(true)
            setTimeout(() => setCopied(false), 1500)
          } catch {}
        }}
      >
        {copied ? <Check /> : <Copy />}
        {copied ? "Copied" : "Copy JSON"}
      </Button>
      <pre tabIndex={0} aria-label="docker inspect output" className="max-h-[70vh] overflow-auto rounded-lg border bg-subtle p-4 font-mono text-xs leading-relaxed">
        {text}
      </pre>
      <p className="mt-1.5 text-xs text-muted-foreground">Env values are masked here; Copy JSON copies them unmasked.</p>
    </div>
  )
}

/** `/containers/[name]`: Overview (state, charts, health, mounts, networks), Logs, Inspect and Env tabs (`?tab=`). Read-only apart from its actions. */
export function ContainerDetail({ name }: { name: string }) {
  const { state } = useDash()
  const tab = useTab(TABS)
  const c = state?.containers.find((x) => x.name === name)
  const [data, setData] = useState<{ name: string; v: Inspect | null; error: string | null } | null>(null)

  useEffect(() => {
    let stop = false
    fetch(`/api/inspect?name=${encodeURIComponent(name)}`, { cache: "no-store" })
      .then(async (r) => {
        const body = await r.json().catch(() => ({ error: r.statusText }))
        if (stop) return
        setData(r.ok && !body.error ? { name, v: body, error: null } : { name, v: null, error: body.error ?? `HTTP ${r.status}` })
      })
      .catch((e) => !stop && setData({ name, v: null, error: String(e) }))
    return () => {
      stop = true
    }
    // Re-read when the container changes state (restart, stop).
  }, [name, c?.state, c?.status])

  const d = data?.name === name ? data.v : null
  const err = data?.name === name ? data.error : null
  const cmd = [...(d?.Config?.Entrypoint ?? []), ...(d?.Config?.Cmd ?? [])].join(" ")
  const health = d?.State?.Health
  const lastHealth = health?.Log?.at(-1)
  const labels = Object.entries(d?.Config?.Labels ?? {}).sort(([a], [b]) => a.localeCompare(b))
  const networks = Object.entries(d?.NetworkSettings?.Networks ?? {})

  return (
    <div className="space-y-6 text-sm">
      <DetailHead
        title={
          <>
            {c && (
              <State tone={containerTone(c.state)}>
                <span className="sr-only">{c.state}</span>
              </State>
            )}
            {name}
          </>
        }
        sub={c ? `${c.image}${c.project ? ` · ${c.project}${c.service ? `/${c.service}` : ""}` : ""}` : undefined}
      >
        {c && <Actions actions={c.actions.filter((a) => a.kind !== "ContainerLogs")} subject={c.name} inline={4} toolbar />}
        <PinButton pin={{ kind: "containers", name }} />
      </DetailHead>
      {c && <TabRow items={TABS.map((t) => (t.value === "env" ? { ...t, badge: d?.Config?.Env?.length } : t))} label="Container views" />}
        {!state ? (
          <Empty>Loading…</Empty>
        ) : !c ? (
          <Empty>No container {name} (it was removed, or the VM is stopped).</Empty>
        ) : (
          <div className="space-y-6">
            {err && <p className="rounded border border-alert-border bg-alert-bg p-2 font-mono text-xs">inspect failed: {err}</p>}
            {tab === "logs" ? (
              <LogView container={c.name} height="calc(100vh - 290px)" className="min-h-[420px]" />
            ) : tab === "inspect" ? (
              <InspectJson value={d} />
            ) : tab === "env" ? (
              <Env env={d?.Config?.Env} loaded={!!d} />
            ) : (
            <>
            <div className="grid gap-6 lg:grid-cols-2">
            <dl className="facts h-fit grid-cols-[7rem_1fr] sm:grid-cols-[8rem_1fr]">
              <dt className="text-muted-foreground">State</dt>
              <dd className="text-xs">
                {c.status || c.state}
                {d?.State?.ExitCode ? ` · exit ${d.State.ExitCode}` : ""}
              </dd>
              <dt className="text-muted-foreground">Command</dt>
              <dd className="font-mono text-xs break-all">{cmd || "–"}</dd>
              {d?.Config?.WorkingDir && (
                <>
                  <dt className="text-muted-foreground">Workdir</dt>
                  <dd className="font-mono text-xs break-all">{d.Config.WorkingDir}</dd>
                </>
              )}
              <dt className="text-muted-foreground">Created</dt>
              <dd className="text-xs">{d?.Created ? `${ago(secs(d.Created))} · ${new Date(d.Created).toLocaleString()}` : "–"}</dd>
              <dt className="text-muted-foreground">Started</dt>
              <dd className="text-xs">{secs(d?.State?.StartedAt) ? ago(secs(d?.State?.StartedAt)) : "–"}</dd>
              <dt className="text-muted-foreground">Restarts</dt>
              <dd className="font-mono text-xs">
                {d?.RestartCount ?? "–"}
                {d?.HostConfig?.RestartPolicy?.Name && d.HostConfig.RestartPolicy.Name !== "no" ? ` (policy ${d.HostConfig.RestartPolicy.Name})` : ""}
              </dd>
              <dt className="text-muted-foreground">Health</dt>
              <dd className="text-xs">
                {health?.Status ? (
                  <State tone={health.Status === "healthy" ? "ok" : health.Status === "starting" ? "info" : "bad"}>
                    {health.Status}
                    {health.FailingStreak ? ` · ${health.FailingStreak} failing` : ""}
                  </State>
                ) : (
                  "no health check"
                )}
              </dd>
            </dl>
            {c.state === "running" ? (
              <Section title="CPU and memory">
                <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-1 xl:grid-cols-2">
                  <ResourceChart metric="cpu" name={c.name} height={140} />
                  <ResourceChart metric="mem" name={c.name} height={140} />
                </div>
              </Section>
            ) : (
              <div />
            )}
            </div>
            {lastHealth && (
              <Section title="Last health check">
                <pre className="max-h-40 overflow-auto rounded-lg border bg-subtle p-2.5 font-mono text-xs whitespace-pre-wrap">
                  exit {lastHealth.ExitCode} · {lastHealth.End ? ago(secs(lastHealth.End)) : ""}
                  {"\n"}
                  {lastHealth.Output?.trim() || "(no output)"}
                </pre>
              </Section>
            )}
            <Section title="Mounts">
              {d?.Mounts?.length ? (
                <ul className="space-y-1 font-mono text-xs">
                  {d.Mounts.map((m) => (
                    <li key={m.Destination} className="break-all">
                      <Badge variant="outline" className="mr-1.5">
                        {m.Type}
                      </Badge>
                      {m.Name ?? m.Source} → {m.Destination}
                      {m.RW === false ? " (ro)" : ""}
                    </li>
                  ))}
                </ul>
              ) : (
                <p className="text-xs text-muted-foreground">{d ? "none" : "loading…"}</p>
              )}
            </Section>
            <Section title="Networks">
              {networks.length ? (
                <ul className="space-y-1 font-mono text-xs">
                  {networks.map(([n, v]) => (
                    <li key={n}>
                      {n} <span className="text-muted-foreground">{v.IPAddress || "no address"}</span>
                    </li>
                  ))}
                </ul>
              ) : (
                <p className="text-xs text-muted-foreground">{d ? "none" : "loading…"}</p>
              )}
            </Section>
            {labels.length > 0 && (
              <details>
                <summary className="h2 cursor-pointer">Labels ({labels.length})</summary>
                <ul className="mt-2 space-y-1 font-mono text-xs">
                  {labels.map(([k, v]) => (
                    <li key={k} className="break-all">
                      <span className="text-muted-foreground">{k}</span> {v}
                    </li>
                  ))}
                </ul>
              </details>
            )}
            </>
            )}
          </div>
        )}
    </div>
  )
}
