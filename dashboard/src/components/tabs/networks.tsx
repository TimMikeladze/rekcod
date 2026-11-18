"use client"

import { memo } from "react"
import { Actions, EngineBar } from "@/components/actions"
import { Empty, Mono } from "@/components/bits"
import { BulkBar, useSelection } from "@/components/bulk"
import { Check, Head, match, NavRow } from "@/components/table"
import { PruneBar } from "@/components/tabs/images"
import { Badge } from "@/components/ui/badge"
import { Table, TableBody, TableCell } from "@/components/ui/table"
import { useDash } from "@/hooks/dash-context"
import { useSort } from "@/hooks/use-sort"
import { ago } from "@/lib/format"
import type { Network } from "@/lib/types"

const created = (n: Network) => (n.created ? Date.parse(n.created) / 1000 : null)
const flags = (n: Network) => [n.builtin && "docker's own", n.internal && "internal", !n.builtin && !n.containers.length && "unused"].filter(Boolean) as string[]

export function Networks({ q }: { q: string }) {
  const { state } = useDash()
  const { sel, toggle, clear, setAll } = useSelection()
  const sort = useSort<Network>({ name: (n) => n.name, created: created })
  const list = state?.networks ?? []
  const rows = sort.apply(list.filter((n) => match(q, n.name, n.driver, n.scope, ...n.containers, ...flags(n))))
  const bar = <EngineBar kinds={["NetworkCreate"]} />
  if (!list.length)
    return (
      <>
        {bar}
        <Empty>{state?.local_errors?.networks ? `Couldn't list networks: ${state.local_errors.networks}` : state?.running ? "No networks." : "The VM is stopped."}</Empty>
      </>
    )
  const ids = rows.filter((n) => !n.builtin).map((n) => n.name)
  const picked = rows.filter((n) => sel.has(n.name))
  return (
    <>
      <EngineBar kinds={["NetworkCreate"]} note={`${list.length} networks, ${list.filter((n) => !n.builtin && !n.containers.length).length} unused`} />
      <PruneBar kinds={["NetworkPrune"]} note="Compose recreates its networks on the next up." />
      {!rows.length ? (
        <Empty>Nothing matches “{q}”.</Empty>
      ) : (
        <Table>
          <Head
            sort={sort}
            select={{ all: picked.length > 0 && picked.length === ids.length, some: picked.length > 0, toggle: () => setAll(ids, picked.length < ids.length) }}
            cols={[["Network", "", "name"], ["Driver", "hidden sm:table-cell"], ["Containers", "hidden sm:table-cell"], ["Created", "hidden md:table-cell", "created"], ""]}
          />
          <TableBody>
            {rows.map((n) => (
              <NetworkRow key={n.id || n.name} n={n} selected={sel.has(n.name)} onTick={toggle} />
            ))}
          </TableBody>
        </Table>
      )}
      <BulkBar items={picked.map((n) => ({ id: n.name, subject: n.name, actions: n.actions }))} noun={["network", "networks"]} onClear={clear} />
    </>
  )
}

const NetworkRow = memo(function NetworkRow({ n, selected, onTick }: { n: Network; selected: boolean; onTick: (id: string) => void }) {
  const at = created(n)
  return (
    <NavRow label={`Network ${n.name}`} selected={selected}>
      <TableCell className="hidden w-8 pr-0 sm:table-cell">
        {!n.builtin && <Check checked={selected} onChange={() => onTick(n.name)} label={`Select ${n.name}`} />}
      </TableCell>
      <TableCell className="whitespace-normal">
        <div className="flex flex-wrap items-center gap-1.5">
          <Mono className="font-medium break-all">{n.name}</Mono>
          {flags(n).map((f) => (
            <Badge key={f} variant="outline">
              {f}
            </Badge>
          ))}
        </div>
        <Mono className="block text-[11px] text-muted-foreground sm:hidden">
          {n.driver}
          {n.containers.length ? ` · ${n.containers.join(", ")}` : ""}
        </Mono>
      </TableCell>
      <TableCell className="hidden sm:table-cell">
        <Mono className="text-muted-foreground">
          {n.driver} · {n.scope}
        </Mono>
      </TableCell>
      <TableCell className="hidden max-w-64 sm:table-cell">
        <Mono className="text-muted-foreground">{n.containers.join(", ") || "–"}</Mono>
      </TableCell>
      <TableCell className="hidden text-xs text-muted-foreground md:table-cell">{at ? ago(at) : "–"}</TableCell>
      <TableCell>
        <Actions actions={n.actions} subject={n.name} />
      </TableCell>
    </NavRow>
  )
})
