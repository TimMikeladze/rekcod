"use client"

import { Fragment } from "react"
import Link from "next/link"
import { ActionButton } from "@/components/actions"
import { Meter, Mono } from "@/components/bits"
import { DetailHead } from "@/components/shell/page-title"
import { shortId } from "@/components/tabs/images"
import { useDash } from "@/hooks/dash-context"
import { ago, fmtBytes } from "@/lib/format"

/** Where the VM's disk goes: `/system/df` totals plus the data disk's size on the Mac. */
export function DiskDetail() {
  const { state } = useDash()
  const d = state?.disk
  const vd = state?.status.disk
  const parts = d
    ? [
        { label: "Images", bytes: d.images, note: "layers, shared ones counted once" },
        { label: "Containers", bytes: d.containers, note: "writable layers" },
        { label: "Volumes", bytes: d.volumes, note: "named and anonymous, snapshots included" },
        { label: "Build cache", bytes: d.build_cache, note: `${fmtBytes(d.build_cache_free)} not in use` },
      ]
    : []
  const actions = [
    ...(state?.vm_actions ?? []).filter((a) => a.kind === "Compact"),
    ...(state?.disk_actions ?? []).filter((a) => a.kind === "BuildCachePrune"),
    ...(state?.disk_actions ?? []).filter((a) => a.kind !== "BuildCachePrune"),
  ]
  return (
    <div className="space-y-6 text-sm">
      <DetailHead
        title="Disk"
        sub={d ? `Measured ${ago(d.checked, state?.generated)} with docker system df (kept for two minutes).` : "Not measured yet; the next full refresh does it."}
      >
        {actions.length > 0 && (
          <div className="flex flex-wrap gap-1">
            {actions.map((a) => (
              <ActionButton key={a.kind} action={a} subject="disk" />
            ))}
          </div>
        )}
      </DetailHead>
        <div className="grid gap-6 lg:grid-cols-2 [&>*]:min-w-0">
          <section>
            <h2 className="h2 mb-3">Data disk</h2>
            {vd ? (
              <dl className="facts grid-cols-[9rem_1fr]">
                <dt className="text-muted-foreground">Used in the VM</dt>
                <dd>
                  <Mono>
                    {fmtBytes(vd.used_in_vm)} of {fmtBytes(vd.size)}
                  </Mono>
                  <Meter value={vd.size && vd.used_in_vm != null ? vd.used_in_vm / vd.size : null} className="mt-1" />
                </dd>
                <dt className="text-muted-foreground">On the Mac</dt>
                <dd>
                  <Mono>{fmtBytes(vd.on_mac)}</Mono>
                  <p className="text-[11px] text-muted-foreground">
                    The file is sparse. Space freed in the VM goes back to macOS when the disk is compacted (hourly, or Compact now).
                  </p>
                </dd>
              </dl>
            ) : (
              <p className="text-xs text-muted-foreground">The VM is stopped.</p>
            )}
          </section>
          <section>
            <h2 className="h2 mb-3">Breakdown</h2>
            {d ? <Breakdown parts={parts} /> : <p className="text-xs text-muted-foreground">No measurement yet.</p>}
          </section>
        </div>
        {d && <Reclaim />}
        <div className="grid gap-6 lg:grid-cols-2 [&>*]:min-w-0">
          <Largest
            title="Largest images"
            href="/images?sort=-size"
            rows={[...(state?.images ?? [])].sort((x, y) => y.size - x.size).slice(0, 5).map((i) => ({ name: i.tags[0] ?? shortId(i.id), bytes: i.size, note: i.containers.length ? i.containers.join(", ") : "unused", list: "images" }))}
          />
          <Largest
            title="Largest volumes"
            href="/volumes?sort=-size"
            rows={(state?.volumes ?? []).filter((v) => v.size != null).sort((x, y) => y.size! - x.size!).slice(0, 5).map((v) => ({ name: v.name, bytes: v.size!, note: v.containers.length ? v.containers.join(", ") : "unused", list: "volumes" }))}
          />
        </div>
    </div>
  )
}

/** Segment colours: the neutral chart ramp, since none of these parts is good or bad. */
const SHADES = ["var(--chart-1)", "var(--chart-2)", "var(--chart-3)", "var(--chart-4)"]

type Part = { label: string; bytes: number; note: string }

/** One bar split by share of the total, then a legend row per part. */
function Breakdown({ parts }: { parts: Part[] }) {
  const total = parts.reduce((n, p) => n + p.bytes, 0) || 1
  return (
    <div className="space-y-3">
      <div className="flex h-3 w-full overflow-hidden rounded-full bg-muted" role="img" aria-label={parts.map((p) => `${p.label} ${fmtBytes(p.bytes)}`).join(", ")}>
        {parts.map((p, i) => (
          <div key={p.label} className="h-full border-r border-background last:border-r-0" style={{ width: `${(p.bytes / total) * 100}%`, background: SHADES[i] }} title={`${p.label}: ${fmtBytes(p.bytes)}`} />
        ))}
      </div>
      <ul className="space-y-2">
        {parts.map((p, i) => (
          <li key={p.label} className="flex items-baseline gap-2">
            <span aria-hidden className="inline-block size-2.5 flex-none translate-y-px rounded-sm" style={{ background: SHADES[i] }} />
            <span>{p.label}</span>
            <span className="truncate text-[11px] text-muted-foreground">{p.note}</span>
            <Mono className="ml-auto whitespace-nowrap tabular-nums">{fmtBytes(p.bytes)}</Mono>
            <Mono className="w-10 flex-none text-right text-muted-foreground tabular-nums">{Math.round((p.bytes / total) * 100)}%</Mono>
          </li>
        ))}
      </ul>
    </div>
  )
}

/** What the prunes would free, from the lists the state already has. */
function Reclaim() {
  const { state } = useDash()
  if (!state?.disk) return null
  const images = state.images.filter((i) => i.dangling || !i.containers.length)
  const volumes = state.volumes.filter((v) => !v.containers.length && v.size != null)
  const rows = [
    { what: `${images.length} unused or dangling image${images.length === 1 ? "" : "s"}`, bytes: images.reduce((n, i) => n + i.size, 0), href: "/images?q=unused", note: "shared layers may stay" },
    { what: `${volumes.length} unused volume${volumes.length === 1 ? "" : "s"}`, bytes: volumes.reduce((n, v) => n + (v.size ?? 0), 0), href: "/volumes?q=unused", note: "snapshots included" },
    { what: "Build cache not in use", bytes: state.disk.build_cache_free, href: null, note: "" },
  ]
  return (
    <section aria-labelledby="reclaim-h">
      <h2 id="reclaim-h" className="h2 mb-3">
        Reclaimable
      </h2>
      <dl className="facts grid-cols-[1fr_auto]">
        {rows.map((r) => (
          <Fragment key={r.what}>
            <dt>
              {r.href ? (
                <Link href={r.href} className="text-foreground hover:underline">
                  {r.what}
                </Link>
              ) : (
                <span className="text-foreground">{r.what}</span>
              )}
              {r.note && <span className="ml-2 text-[11px]">{r.note}</span>}
            </dt>
            <dd className="pr-3.5 text-right">
              <Mono className="whitespace-nowrap tabular-nums">{fmtBytes(r.bytes)}</Mono>
            </dd>
          </Fragment>
        ))}
      </dl>
    </section>
  )
}

function Largest({ title, href, rows }: { title: string; href: string; rows: { name: string; bytes: number; note: string; list: string }[] }) {
  const top = Math.max(1, ...rows.map((r) => r.bytes))
  return (
    <section>
      <div className="mb-3 flex items-baseline justify-between">
        <h2 className="h2">{title}</h2>
        <Link href={href} className="cap hover:text-foreground">
          All, by size →
        </Link>
      </div>
      {!rows.length ? (
        <p className="text-xs text-muted-foreground">Nothing measured yet.</p>
      ) : (
        <ul className="overflow-hidden rounded-lg border bg-card">
          {rows.map((r) => (
            <li key={r.name} className="border-b border-hairline last:border-b-0">
              <Link href={`/${r.list}?q=${encodeURIComponent(r.name)}`} className="block px-3.5 py-2 hover:bg-subtle">
                <div className="flex items-baseline gap-2">
                  <Mono className="min-w-0 truncate font-medium">{r.name}</Mono>
                  <span className="truncate text-[11px] text-muted-foreground">{r.note}</span>
                  <Mono className="ml-auto whitespace-nowrap tabular-nums">{fmtBytes(r.bytes)}</Mono>
                </div>
                {/* Relative to the largest, so not a usage meter: no warning colours. */}
                <div className="mt-1 h-1 w-full overflow-hidden rounded-full bg-muted" aria-hidden>
                  <div className="h-full rounded-full bg-foreground/50" style={{ width: `${(r.bytes / top) * 100}%` }} />
                </div>
              </Link>
            </li>
          ))}
        </ul>
      )}
    </section>
  )
}
