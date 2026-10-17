"use client"

import { memo, useState } from "react"
import { useRouter } from "next/navigation"
import { ChevronDown, ChevronRight } from "lucide-react"
import { Actions } from "@/components/actions"
import { containerTone, Empty, Meter, Mono, State } from "@/components/bits"
import { BulkBar, useSelection } from "@/components/bulk"
import { GetStarted } from "@/components/get-started"
import { Check, Head, match, NavRow } from "@/components/table"
import { useSort } from "@/hooks/use-sort"
import { Table, TableBody, TableCell, TableRow } from "@/components/ui/table"
import { useDash } from "@/hooks/dash-context"
import { fmtBytes } from "@/lib/format"
import type { Container, Project, Stat } from "@/lib/types"

/** Columns left of the actions one, the checkbox included. */
const COLS = 7

type Group = { key: string; title: string; project: Project | null; rows: Container[]; total: number; running: number }

/** Compose projects first (by name), then everything else. */
function groups(list: Container[], projects: Project[], q: string): Group[] {
  const by = new Map<string, Container[]>()
  for (const c of list) {
    const k = c.project ?? ""
    by.set(k, [...(by.get(k) ?? []), c])
  }
  const out: Group[] = []
  for (const [k, all] of [...by].sort(([a], [b]) => (a === "" ? 1 : b === "" ? -1 : a.localeCompare(b)))) {
    const rows = all.filter((c) => match(q, c.name, c.image, c.project, c.service, c.state, ...c.ports))
    if (!rows.length) continue
    out.push({
      key: k || "-",
      title: k || "Other containers",
      project: k ? (projects.find((p) => p.name === k) ?? null) : null,
      rows,
      total: all.length,
      running: all.filter((c) => c.state === "running").length,
    })
  }
  return out
}

export function Containers({ q }: { q: string }) {
  const { state, stats } = useDash()
  const [closed, setClosed] = useState<Set<string>>(new Set())
  const { sel, toggle: tick, clear, setAll } = useSelection()
  const sort = useSort<Container>({
    name: (c) => c.name,
    cpu: (c) => stats[c.name]?.cpu ?? null,
    mem: (c) => stats[c.name]?.mem ?? null,
    status: (c) => c.state,
  })
  const list = state?.containers ?? []
  const err = state?.local_errors?.containers
  if (!list.length && err) return <Empty>Couldn&apos;t list containers: {err}</Empty>
  if (!list.length)
    return (
      <GetStarted title="No containers yet" commands={[["run", "-d", "-p", "8080:80", "nginx"], ["compose", "up", "-d"]]}>
        Anything dockerd runs in the VM shows up here, grouped by compose project.
      </GetStarted>
    )
  const gs = groups(list, state?.projects ?? [], q).map((g) => ({ ...g, rows: sort.apply(g.rows) }))
  if (!gs.length) return <Empty>Nothing matches “{q}”.</Empty>
  // One group with no project (no compose at all) needs no header.
  const headers = gs.length > 1 || gs[0].project != null
  const toggle = (k: string) =>
    setClosed((s) => {
      const n = new Set(s)
      if (n.has(k)) n.delete(k)
      else n.add(k)
      return n
    })
  const shownIds = gs.flatMap((g) => g.rows.map((c) => c.id))
  const picked = list.filter((c) => sel.has(c.id) && shownIds.includes(c.id))
  return (
    <>
      <Table>
        <Head
          sort={sort}
          select={{ all: picked.length > 0 && picked.length === shownIds.length, some: picked.length > 0, toggle: () => setAll(shownIds, picked.length < shownIds.length) }}
          cols={[
            ["Name", "", "name"],
            ["Image", "hidden md:table-cell"],
            ["Ports", "hidden sm:table-cell"],
            ["CPU", "w-16 text-right", "cpu"],
            ["Memory", "hidden w-36 sm:table-cell", "mem"],
            ["Status", "hidden lg:table-cell", "status"],
            "",
          ]}
        />
        {gs.map((g) => {
          const shut = closed.has(g.key) && !q
          return (
            <TableBody key={g.key} className="border-b last:border-b-0">
              {headers && (
                <TableRow className="bg-subtle hover:bg-subtle">
                  {/* One cell: a separate actions cell would add a column on phones, where the others are hidden. */}
                  <TableCell colSpan={COLS + 1} className="py-1.5">
                    <div className="flex items-center gap-2">
                      <button
                        type="button"
                        onClick={() => toggle(g.key)}
                        aria-expanded={!shut}
                        className="flex items-center gap-1.5 text-left text-xs font-medium"
                      >
                        {shut ? <ChevronRight className="size-3.5" /> : <ChevronDown className="size-3.5" />}
                        <span>{g.title}</span>
                        <Mono className="text-muted-foreground tabular-nums">
                          {g.running}/{g.total} running
                        </Mono>
                      </button>
                      {g.project && (
                        <div className="ml-auto">
                          <Actions actions={g.project.actions} subject={g.title} inline={1} />
                        </div>
                      )}
                    </div>
                  </TableCell>
                </TableRow>
              )}
              {!shut && g.rows.map((c) => <Row key={c.id} c={c} s={stats[c.name]} selected={sel.has(c.id)} onTick={tick} />)}
            </TableBody>
          )
        })}
      </Table>
      <BulkBar items={picked.map((c) => ({ id: c.id, subject: c.name, actions: c.actions }))} noun={["container", "containers"]} onClear={clear} />
    </>
  )
}

/** One container. Memoised: shared state keeps `c` and `s` the same object while nothing in them changed. */
const Row = memo(function Row({ c, s, selected, onTick }: { c: Container; s?: Stat; selected: boolean; onTick: (id: string) => void }) {
  const router = useRouter()
  return (
    <NavRow onOpen={() => router.push(`/containers/${encodeURIComponent(c.name)}`)} label={`${c.name}, ${c.state}`} watch={c.state} selected={selected}>
      <TableCell className="hidden w-8 pr-0 sm:table-cell">
        <Check checked={selected} onChange={() => onTick(c.id)} label={`Select ${c.name}`} />
      </TableCell>
      <TableCell>
        <State tone={containerTone(c.state)}>
          <span className="sr-only">{c.state}: </span>
          <span className="block max-w-48 truncate font-medium sm:max-w-72" title={c.name}>
            {c.name}
          </span>
        </State>
        {/* Phones: the hidden columns' gist under the name. */}
        <Mono className="block max-w-52 truncate pl-3.5 text-[11px] text-muted-foreground md:hidden">
          {c.service ?? c.image}
          {c.ports.length ? ` · :${c.ports.join(" :")}` : ""}
          <span className="sm:hidden"> · {c.status || c.state}</span>
        </Mono>
      </TableCell>
      <TableCell className="hidden max-w-56 truncate md:table-cell" title={c.image}>
        <Mono className="text-muted-foreground">{c.image}</Mono>
      </TableCell>
      <TableCell className="hidden sm:table-cell">
        <div className="flex flex-wrap gap-1">
          {c.ports.map((p) => (
            <a
              key={p}
              href={`http://localhost:${p}`}
              target="_blank"
              rel="noreferrer"
              onClick={(e) => e.stopPropagation()}
              className="font-mono text-xs underline-offset-2 hover:underline"
            >
              :{p}
            </a>
          ))}
        </div>
      </TableCell>
      <TableCell className="text-right tabular-nums">
        <Mono>{s ? `${s.cpu.toFixed(1)}%` : "–"}</Mono>
      </TableCell>
      <TableCell className="hidden sm:table-cell">
        {s ? (
          <div className="space-y-1">
            <Mono>{fmtBytes(s.mem)}</Mono>
            <Meter value={s.memLimit ? s.mem / s.memLimit : null} />
          </div>
        ) : (
          <Mono className="text-muted-foreground">–</Mono>
        )}
      </TableCell>
      <TableCell className="hidden text-xs whitespace-nowrap text-muted-foreground lg:table-cell">{c.status}</TableCell>
      <TableCell>
        <Actions actions={c.actions} subject={c.name} />
      </TableCell>
    </NavRow>
  )
})
