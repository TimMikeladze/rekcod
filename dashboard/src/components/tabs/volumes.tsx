"use client"

import { Actions } from "@/components/actions"
import { Empty, Mono } from "@/components/bits"
import { memo } from "react"
import { BulkBar, useSelection } from "@/components/bulk"
import { Check, Head, match, NavRow } from "@/components/table"
import { useSort } from "@/hooks/use-sort"
import { PruneBar } from "@/components/tabs/images"
import { Badge } from "@/components/ui/badge"
import { Table, TableBody, TableCell } from "@/components/ui/table"
import { useDash } from "@/hooks/dash-context"
import { fmtBytes } from "@/lib/format"
import type { Volume } from "@/lib/types"

const flags = (v: Volume) => [v.snapshot && "snapshot", v.anonymous && "anonymous", !v.containers.length && "unused"].filter(Boolean) as string[]

export function Volumes({ q }: { q: string }) {
  const { state } = useDash()
  const { sel, toggle, clear, setAll } = useSelection()
  const sort = useSort<Volume>({ name: (v) => v.name, size: (v) => v.size })
  const list = state?.volumes ?? []
  const rows = sort.apply(list.filter((v) => match(q, v.name, v.driver, ...v.containers, ...flags(v))))
  const known = list.filter((v) => v.size != null)
  const total = known.reduce((a, v) => a + (v.size ?? 0), 0)
  if (!list.length) return <Empty>{state?.local_errors?.volumes ? `Couldn't list volumes: ${state.local_errors.volumes}` : state?.running ? "No volumes." : "The VM is stopped."}</Empty>
  const ids = rows.map((v) => v.name)
  const picked = rows.filter((v) => sel.has(v.name))
  return (
    <>
      <PruneBar
        kinds={["VolumePrune"]}
        note={`${list.length} volumes${known.length ? `, ${fmtBytes(total)}` : ""}${known.length < list.length ? " (sizes come from the last disk check)" : ""}`}
      />
      {!rows.length ? (
        <Empty>Nothing matches “{q}”.</Empty>
      ) : (
        <Table>
          <Head
            sort={sort}
            select={{ all: picked.length > 0 && picked.length === ids.length, some: picked.length > 0, toggle: () => setAll(ids, picked.length < ids.length) }}
            cols={[["Volume", "", "name"], ["Size", "w-24 text-right", "size"], ["Used by", "hidden sm:table-cell"], ["Driver", "hidden md:table-cell"], ""]}
          />
          <TableBody>
            {rows.map((v) => (
              <VolumeRow key={v.name} v={v} selected={sel.has(v.name)} onTick={toggle} />
            ))}
          </TableBody>
        </Table>
      )}
      <BulkBar items={picked.map((v) => ({ id: v.name, subject: v.name, actions: v.actions }))} noun={["volume", "volumes"]} onClear={clear} />
    </>
  )
}

const VolumeRow = memo(function VolumeRow({ v, selected, onTick }: { v: Volume; selected: boolean; onTick: (id: string) => void }) {
  return (
    <NavRow label={`Volume ${v.name}`} selected={selected}>
      <TableCell className="hidden w-8 pr-0 sm:table-cell">
        <Check checked={selected} onChange={() => onTick(v.name)} label={`Select ${v.name}`} />
      </TableCell>
      <TableCell className="whitespace-normal">
        <div className="flex flex-wrap items-center gap-1.5">
          <Mono className="font-medium break-all">{v.name}</Mono>
          {flags(v).map((f) => (
            <Badge key={f} variant="outline">
              {f}
            </Badge>
          ))}
        </div>
        {v.containers.length > 0 && <Mono className="block text-[11px] text-muted-foreground sm:hidden">{v.containers.join(", ")}</Mono>}
      </TableCell>
      <TableCell className="text-right">
        <Mono>{v.size != null ? fmtBytes(v.size) : "–"}</Mono>
      </TableCell>
      <TableCell className="hidden max-w-64 sm:table-cell">
        <Mono className="text-muted-foreground">{v.containers.join(", ") || "–"}</Mono>
      </TableCell>
      <TableCell className="hidden md:table-cell">
        <Mono className="text-muted-foreground">{v.driver}</Mono>
      </TableCell>
      <TableCell>
        <Actions actions={v.actions} subject={v.name} />
      </TableCell>
    </NavRow>
  )
})
