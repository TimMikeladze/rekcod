"use client"

import { useSearchParams } from "next/navigation"
import { Empty, Mono } from "@/components/bits"
import { PageTitle } from "@/components/shell/page-title"
import { LogView } from "@/components/log-sheet"
import { useDash } from "@/hooks/dash-context"
import { findOffered } from "@/lib/allow"

/**
 * Output of a log-like action (`app logs`, `job logs`, `sandbox diff`, `storage env`) as a
 * page. The argv must be one the current state offers, as the server checks too.
 */
export function StreamPage() {
  const params = useSearchParams()
  const { state } = useDash()
  let argv: unknown = null
  try {
    argv = JSON.parse(params.get("argv") ?? "null")
  } catch {}
  const action = findOffered(state, argv)
  const title = params.get("title") ?? "Output"
  return (
    <div className="flex min-h-[70vh] flex-col gap-4">
      <PageTitle title={title}>
        <Mono>rekcod {Array.isArray(argv) ? argv.join(" ") : ""}</Mono>
      </PageTitle>
      {action ? (
        <LogView action={action} height="70vh" />
      ) : (
        <Empty>That isn&apos;t something rekcod offers right now (the item may be gone).</Empty>
      )}
    </div>
  )
}
