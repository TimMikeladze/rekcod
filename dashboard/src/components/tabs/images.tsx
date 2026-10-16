"use client"

import { ActionButton, Actions } from "@/components/actions"
import { Empty, Mono } from "@/components/bits"
import { memo } from "react"
import { BulkBar, useSelection } from "@/components/bulk"
import { Check, Head, match, NavRow } from "@/components/table"
import { useSort } from "@/hooks/use-sort"
import { Badge } from "@/components/ui/badge"
import { Table, TableBody, TableCell } from "@/components/ui/table"
import { useDash } from "@/hooks/dash-context"
import { ago, fmtBytes } from "@/lib/format"
import type { Action, Image } from "@/lib/types"

/** The prunes (`disk_actions`) above the Images and Volumes tables. */
export function PruneBar({ kinds, note }: { kinds: string[]; note: string }) {
  const { state } = useDash()
  const actions: Action[] = (state?.disk_actions ?? []).filter((a) => kinds.includes(a.kind))
  if (!actions.length) return null
  return (
    <div className="flex flex-wrap items-center gap-2 px-2 py-2 text-xs text-muted-foreground">
      <span>{note}</span>
      <div className="ml-auto flex flex-wrap gap-1">
        {actions.map((a) => (
          <ActionButton key={a.kind} action={a} subject="disk" />
        ))}
      </div>
    </div>
  )
}

export const shortId = (id: string) => id.replace(/^sha256:/, "").slice(0, 12)

export function Images({ q }: { q: string }) {
  const { state } = useDash()
  const { sel, toggle, clear, setAll } = useSelection()
  const sort = useSort<Image>({ name: (i) => i.tags[0] ?? null, size: (i) => i.size, created: (i) => i.created })
  const list = state?.images ?? []
  const rows = sort.apply(list.filter((i) => match(q, i.id, ...i.tags, ...i.containers, i.dangling ? "dangling" : "", i.containers.length ? "" : "unused")))
  const total = list.reduce((a, i) => a + i.size, 0)
  if (!list.length) return <Empty>{state?.running ? "No images." : "The VM is stopped."}</Empty>
  const ids = rows.map((i) => i.id)
  const picked = rows.filter((i) => sel.has(i.id))
  return (
    <>
      <PruneBar
        kinds={["ImagePrune"]}
        note={`${list.length} images, ${fmtBytes(total)} (layers shared between images count once on disk)`}
      />
      {!rows.length ? (
        <Empty>Nothing matches “{q}”.</Empty>
      ) : (
        <Table>
          <Head
            sort={sort}
            select={{ all: picked.length > 0 && picked.length === ids.length, some: picked.length > 0, toggle: () => setAll(ids, picked.length < ids.length) }}
            cols={[["Image", "", "name"], ["Size", "w-24 text-right", "size"], ["Used by", "hidden sm:table-cell"], ["Created", "hidden md:table-cell", "created"], ""]}
          />
          <TableBody>
            {rows.map((i) => (
              <ImageRow key={i.id} i={i} selected={sel.has(i.id)} onTick={toggle} />
            ))}
          </TableBody>
        </Table>
      )}
      <BulkBar items={picked.map((i) => ({ id: i.id, subject: i.tags[0] ?? shortId(i.id), actions: i.actions }))} noun={["image", "images"]} onClear={clear} />
    </>
  )
}

const ImageRow = memo(function ImageRow({ i, selected, onTick }: { i: Image; selected: boolean; onTick: (id: string) => void }) {
  const name = i.tags[0] ?? shortId(i.id)
  return (
    <NavRow label={`Image ${name}`} selected={selected}>
      <TableCell className="hidden w-8 pr-0 sm:table-cell">
        <Check checked={selected} onChange={() => onTick(i.id)} label={`Select ${name}`} />
      </TableCell>
      <TableCell>
        <div className="flex flex-wrap items-center gap-1.5">
          <Mono className="font-medium break-all">{i.tags[0] ?? "<none>"}</Mono>
          {i.dangling && <Badge variant="outline">dangling</Badge>}
          {!i.containers.length && !i.dangling && <Badge variant="outline">unused</Badge>}
        </div>
        {i.tags.length > 1 && <Mono className="block text-[11px] text-muted-foreground">{i.tags.slice(1).join(", ")}</Mono>}
        <Mono className="block text-[11px] text-muted-foreground">
          {shortId(i.id)}
          <span className="sm:hidden">{i.containers.length ? ` · ${i.containers.join(", ")}` : ""}</span>
        </Mono>
      </TableCell>
      <TableCell className="text-right">
        <Mono>{fmtBytes(i.size)}</Mono>
      </TableCell>
      <TableCell className="hidden max-w-64 sm:table-cell">
        <Mono className="text-muted-foreground">{i.containers.join(", ") || "–"}</Mono>
      </TableCell>
      <TableCell className="hidden text-xs text-muted-foreground md:table-cell">{ago(i.created)}</TableCell>
      <TableCell>
        <Actions actions={i.actions} subject={name} />
      </TableCell>
    </NavRow>
  )
})
