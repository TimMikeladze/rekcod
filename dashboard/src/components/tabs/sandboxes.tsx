"use client"

import { useRouter } from "next/navigation"
import { Actions } from "@/components/actions"
import { Empty, Mono, State } from "@/components/bits"
import { Head, match, NavRow } from "@/components/table"
import { Badge } from "@/components/ui/badge"
import { Table, TableBody, TableCell } from "@/components/ui/table"
import { useDash } from "@/hooks/dash-context"
import { ago, fmtBytes } from "@/lib/format"

export function Sandboxes({ q }: { q: string }) {
  const { state } = useDash()
  const router = useRouter()
  const list = state?.sandboxes ?? []
  const rows = list.filter((s) => match(q, s.id, s.name, s.project, s.macos ? "macos" : "linux"))
  if (!list.length) return <Empty>No agent sandboxes. `rekcod sandbox create` makes one.</Empty>
  if (!rows.length) return <Empty>Nothing matches “{q}”.</Empty>
  return (
    <>
      <Table>
        <Head cols={["Sandbox", ["Project", "hidden md:table-cell"], ["Kind", "hidden sm:table-cell"], "Using", ["Age", "hidden sm:table-cell"], "State", ""]} />
        <TableBody>
          {rows.map((s) => (
            <NavRow key={s.id} onOpen={() => router.push(`/sandboxes/${encodeURIComponent(s.id)}`)} label={`Sandbox ${s.name ?? s.id}, ${s.running ? "running" : "stopped"}`}>
              <TableCell>
                <div className="font-medium">{s.name ?? <Mono>{s.id}</Mono>}</div>
                {s.name && <Mono className="text-muted-foreground">{s.id}</Mono>}
                <Mono className="block text-[11px] text-muted-foreground sm:hidden">
                  {s.macos ? "macOS" : "linux"} · {ago(s.created)}
                </Mono>
              </TableCell>
              <TableCell className="hidden max-w-72 truncate md:table-cell" title={s.project}>
                <Mono className="text-muted-foreground">{s.project}</Mono>
              </TableCell>
              <TableCell className="hidden space-x-1 sm:table-cell">
                <Badge variant="outline">{s.macos ? "macOS" : "linux"}</Badge>
                <Badge variant="outline">{s.copy ? "copy" : "mount"}</Badge>
              </TableCell>
              <TableCell>
                {s.cpu_pct != null ? (
                  <Mono>
                    {s.cpu_pct.toFixed(0)}% · {s.rss_mib != null ? fmtBytes(s.rss_mib * 1024 * 1024) : "–"}
                  </Mono>
                ) : (
                  <Mono className="text-muted-foreground">–</Mono>
                )}
                <Mono className="hidden text-[11px] text-muted-foreground sm:block">
                  of {s.cpus} CPU · {s.memory_gib} GiB
                </Mono>
              </TableCell>
              <TableCell className="hidden text-xs text-muted-foreground sm:table-cell">{ago(s.created)}</TableCell>
              <TableCell>
                <State tone={s.running ? "ok" : "idle"}>
                  {/* Phones keep the dot; the word is read out. */}
                  <span className="max-sm:sr-only">{s.running ? "running" : "stopped"}</span>
                </State>
              </TableCell>
              <TableCell>
                <Actions actions={s.actions} subject={s.name ?? s.id} />
              </TableCell>
            </NavRow>
          ))}
        </TableBody>
      </Table>
    </>
  )
}
