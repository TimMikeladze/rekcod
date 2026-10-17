"use client"

import { useEffect, useState } from "react"
import { Actions } from "@/components/actions"
import { Empty, Mono, State } from "@/components/bits"
import { DetailHead } from "@/components/shell/page-title"
import { useDash } from "@/hooks/dash-context"
import { ago, fmtBytes } from "@/lib/format"

/** `/sandboxes/[id]`: changed files (`rekcod sandbox diff --stat`) and, for macOS, a screenshot every 10 s. */
export function SandboxDetail({ id }: { id: string }) {
  const { state } = useDash()
  const s = state?.sandboxes.find((x) => x.id === id)
  const [stat, setStat] = useState<{ id: string; text: string } | null>(null)
  const [shot, setShot] = useState<{ url: string; at: number; error: string | null } | null>(null)

  useEffect(() => {
    if (!s?.copy) return
    let stop = false
    fetch(`/api/sandbox-stat?id=${encodeURIComponent(id)}`, { cache: "no-store" })
      .then((r) => r.text())
      .then((text) => !stop && setStat({ id, text }))
      .catch((e) => !stop && setStat({ id, text: String(e) }))
    return () => {
      stop = true
    }
  }, [id, s?.copy])

  const screenshots = !!s?.macos && !!s.running
  useEffect(() => {
    if (!screenshots) return
    let stop = false
    let last: string | null = null
    const tick = async () => {
      try {
        const r = await fetch(`/api/screenshot?id=${encodeURIComponent(id)}&t=${Date.now()}`, { cache: "no-store" })
        if (!r.ok) throw new Error((await r.text()) || `HTTP ${r.status}`)
        const url = URL.createObjectURL(await r.blob())
        if (stop) return URL.revokeObjectURL(url)
        if (last) URL.revokeObjectURL(last)
        last = url
        setShot({ url, at: Date.now(), error: null })
      } catch (e) {
        if (!stop) setShot((p) => ({ url: p?.url ?? "", at: p?.at ?? 0, error: String((e as Error).message ?? e) }))
      }
    }
    tick()
    const t = setInterval(tick, 10_000)
    return () => {
      stop = true
      clearInterval(t)
      if (last) URL.revokeObjectURL(last)
      setShot(null)
    }
  }, [screenshots, id])

  const text = stat?.id === id ? stat.text : null
  return (
    <div className="space-y-6 text-sm">
      <DetailHead title={s?.name ?? id} sub={s ? `${s.id} · ${s.project}` : undefined}>
        {s && <Actions actions={s.actions} subject={s.name ?? s.id} inline={3} toolbar />}
      </DetailHead>
        {!state ? (
          <Empty>Loading…</Empty>
        ) : !s ? (
          <Empty>No sandbox {id}; it was deleted.</Empty>
        ) : (
          <div className="grid gap-6 lg:grid-cols-2">
            <dl className="facts h-fit grid-cols-[8rem_1fr]">
              <dt className="text-muted-foreground">State</dt>
              <dd>
                <State tone={s.running ? "ok" : "idle"}>{s.running ? "running" : "stopped"}</State>
              </dd>
              <dt className="text-muted-foreground">Kind</dt>
              <dd className="text-xs">
                {s.macos ? "macOS" : "Linux"} · {s.copy ? "copy of the project" : "project mounted"}
              </dd>
              <dt className="text-muted-foreground">Created</dt>
              <dd className="text-xs">{ago(s.created)}</dd>
              <dt className="text-muted-foreground">Allotted</dt>
              <dd className="font-mono text-xs">
                {s.cpus} CPU · {s.memory_gib} GiB
              </dd>
              <dt className="text-muted-foreground">Using</dt>
              <dd className="font-mono text-xs">
                {s.cpu_pct != null ? `${s.cpu_pct.toFixed(1)}% CPU · ${s.rss_mib != null ? fmtBytes(s.rss_mib * 1024 * 1024) : "? memory"}` : "–"}
              </dd>
            </dl>
            {s.macos && (
              <section>
                <h2 className="h2 mb-3">Screen</h2>
                {!s.running ? (
                  <p className="text-xs text-muted-foreground">Stopped; no screen.</p>
                ) : shot?.url ? (
                  <figure>
                    {/* A blob URL from the screenshot route; next/image can't optimize it. */}
                    {/* eslint-disable-next-line @next/next/no-img-element */}
                    <img src={shot.url} alt={`Screen of sandbox ${s.name ?? s.id}`} className="w-full rounded-md border" />
                    <figcaption className="mt-1 text-[11px] text-muted-foreground">
                      {shot.error ? `last refresh failed: ${shot.error}` : `taken ${ago(shot.at / 1000)}, refreshes every 10 s`}
                    </figcaption>
                  </figure>
                ) : (
                  <p className="text-xs text-muted-foreground">{shot?.error ? `screenshot failed: ${shot.error}` : "taking a screenshot…"}</p>
                )}
              </section>
            )}
            {s.copy && (
              <section>
                <h2 className="h2 mb-3">Files changed</h2>
                <pre className="max-h-[32rem] overflow-auto rounded-lg border bg-subtle p-3 font-mono text-xs whitespace-pre-wrap">
                  {text == null ? "reading…" : text.trim() || "no changes"}
                </pre>
                <Mono className="mt-1 block text-[11px] text-muted-foreground">rekcod sandbox diff --stat {s.id}</Mono>
              </section>
            )}
          </div>
        )}
    </div>
  )
}
