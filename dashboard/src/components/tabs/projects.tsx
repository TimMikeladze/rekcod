"use client"

import { Actions } from "@/components/actions"
import { Empty, Mono } from "@/components/bits"
import { Head, match } from "@/components/table"
import { Badge } from "@/components/ui/badge"
import { Table, TableBody, TableCell, TableRow } from "@/components/ui/table"
import { useDash } from "@/hooks/dash-context"
import { ago, fmtBytes } from "@/lib/format"

export function Projects({ q }: { q: string }) {
  const list = useDash().state?.projects ?? []
  const rows = list.filter((p) => match(q, p.name, p.dir, ...p.snapshots.map((s) => s.name)))
  if (!list.length) return <Empty>No compose projects or snapshots. `rekcod snap save` in a compose project saves its volumes.</Empty>
  if (!rows.length) return <Empty>Nothing matches “{q}”.</Empty>
  return (
    <div className="divide-y">
      {rows.map((p) => (
        <section key={p.name} className="py-3">
          <div className="flex flex-wrap items-center gap-3 px-2">
            <div>
              <div className="font-medium">
                {p.name}
                <Mono className="ml-2 font-normal text-muted-foreground">
                  {p.containers ? `${p.running}/${p.containers} running` : "no containers"}
                  {p.branch ? ` · branch ${p.branch}` : ""}
                </Mono>
              </div>
              <Mono className="break-all text-muted-foreground">{p.dir ?? "no containers left"}</Mono>
            </div>
            <div className="ml-auto">
              <Actions actions={[...p.actions].sort((a, b) => Number(b.kind === "SnapSave") - Number(a.kind === "SnapSave"))} subject={p.name} inline={1} />
            </div>
          </div>
          {p.snapshots.length > 0 && (
            <Table className="mt-2">
              <Head cols={["Snapshot", ["Volumes", "hidden sm:table-cell"], ["Size", "text-right"], "Saved", ""]} />
              <TableBody>
                {p.snapshots.map((s) => (
                  <TableRow key={s.name}>
                    <TableCell>
                      <Mono className="font-medium">{s.name}</Mono>
                      {s.current && (
                        <Badge variant="secondary" className="ml-2" title="Matches the project's git branch: `snap restore` without a name picks this one">
                          current branch
                        </Badge>
                      )}
                    </TableCell>
                    <TableCell className="hidden sm:table-cell">
                      <Mono className="text-muted-foreground">{s.volumes.join(", ")}</Mono>
                    </TableCell>
                    <TableCell className="text-right">
                      <Mono>{s.size != null ? fmtBytes(s.size) : "–"}</Mono>
                    </TableCell>
                    <TableCell className="text-xs text-muted-foreground">{ago(s.created)}</TableCell>
                    <TableCell>
                      <Actions actions={s.actions} subject={`${p.name}/${s.name}`} inline={1} />
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          )}
        </section>
      ))}
    </div>
  )
}
