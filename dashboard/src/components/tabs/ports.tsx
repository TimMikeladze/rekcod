"use client"

import { useEffect, useState } from "react"
import { Actions } from "@/components/actions"
import { Empty, Mono, State } from "@/components/bits"
import { Head, match } from "@/components/table"
import { Badge } from "@/components/ui/badge"
import { Table, TableBody, TableCell, TableRow } from "@/components/ui/table"
import { useDash } from "@/hooks/dash-context"

/** `GET /api/probe`: per TCP port, the HEAD status or why there's none. */
type Probe = Record<string, { status: number | null; note: string }>

function useProbe(ports: number[]) {
  const [probe, setProbe] = useState<Probe>({})
  const key = ports.join(",")
  useEffect(() => {
    if (!key) return
    let stop = false
    const tick = () =>
      fetch("/api/probe", { cache: "no-store" })
        .then((r) => (r.ok ? r.json() : {}))
        .then((p) => !stop && setProbe(p))
        .catch(() => {})
    tick()
    // The server caches each answer for 10 s.
    const id = setInterval(() => document.visibilityState === "visible" && tick(), 10_000)
    return () => {
      stop = true
      clearInterval(id)
    }
  }, [key])
  return probe
}

function HttpCell({ p }: { p?: { status: number | null; note: string } }) {
  if (!p) return <Mono className="text-muted-foreground">…</Mono>
  if (p.status == null) return <Mono className="text-muted-foreground">{p.note || "not HTTP"}</Mono>
  const tone = p.status < 400 ? "ok" : p.status < 500 ? "warn" : "bad"
  return (
    <State tone={tone}>
      <Mono>HTTP {p.status}</Mono>
    </State>
  )
}

export function Ports({ q }: { q: string }) {
  const { state } = useDash()
  const list = state?.ports ?? []
  const probe = useProbe(list.filter((p) => !p.udp && !p.error).map((p) => p.port))
  const owner = (name: string) => state?.containers.find((c) => c.name === name)
  const rows = list.filter((p) => match(q, p.port, p.container, p.error, owner(p.container)?.project, owner(p.container)?.service))
  if (!list.length) return <Empty>No published ports.</Empty>
  if (!rows.length) return <Empty>Nothing matches “{q}”.</Empty>
  return (
    <Table>
      <Head cols={["Port", "Container", ["Compose", "hidden md:table-cell"], "Forwarded", ["Answers", "hidden sm:table-cell"], ""]} />
      <TableBody>
        {rows.map((p) => {
          const c = owner(p.container)
          const pr = probe[String(p.port)]
          return (
            <TableRow key={`${p.port}/${p.udp}/${p.sandbox}`}>
              <TableCell>
                <Mono className="font-medium">
                  {p.port}
                  {p.udp ? "/udp" : ""}
                </Mono>
                {p.sandbox && (
                  <Badge variant="outline" className="ml-2">
                    sandbox VM
                  </Badge>
                )}
              </TableCell>
              <TableCell className="max-w-48 truncate" title={p.container}>
                {p.container}
                {c?.project && (
                  <Mono className="block text-[11px] text-muted-foreground md:hidden">
                    {c.project}/{c.service ?? "?"}
                  </Mono>
                )}
              </TableCell>
              <TableCell className="hidden md:table-cell">
                <Mono className="text-muted-foreground">{c?.project ? `${c.project}/${c.service ?? "?"}` : "–"}</Mono>
              </TableCell>
              <TableCell>{p.error ? <State tone="bad">{p.error}</State> : <State tone="ok">localhost:{p.port}</State>}</TableCell>
              <TableCell className="hidden sm:table-cell">
                {p.udp ? <Mono className="text-muted-foreground">UDP</Mono> : p.error ? <Mono className="text-muted-foreground">–</Mono> : <HttpCell p={pr} />}
              </TableCell>
              <TableCell>
                <Actions actions={p.actions} subject={`:${p.port}`} />
              </TableCell>
            </TableRow>
          )
        })}
      </TableBody>
    </Table>
  )
}
