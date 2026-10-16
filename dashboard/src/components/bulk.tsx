"use client"

import { useCallback, useEffect, useState } from "react"
import { X } from "lucide-react"
import { short, useAction } from "@/components/actions"
import { Button } from "@/components/ui/button"
import { DESTRUCTIVE, STREAMED } from "@/lib/kinds"
import type { Action } from "@/lib/types"
import { cn } from "@/lib/utils"

/** One row a bulk action can act on: its id, what toasts call it, its own offered actions. */
export type BulkItem = { id: string; subject: string; actions: Action[] }

/**
 * Action kinds every item offers and that run without a terminal or a stream: what the bulk
 * bar can do to all of them. In the first item's order.
 */
export function commonKinds(items: BulkItem[]): Action[] {
  if (!items.length) return []
  const ok = (a: Action) => !!a.argv && !a.url && !a.interactive && !STREAMED.has(a.kind)
  return items[0].actions.filter((a) => ok(a) && items.every((i) => i.actions.some((b) => b.kind === a.kind && ok(b))))
}

/** Ticked row ids. `toggle` and `clear` keep their identity, so memoised rows don't re-render. */
export function useSelection() {
  const [sel, setSel] = useState<ReadonlySet<string>>(new Set())
  const toggle = useCallback(
    (id: string) =>
      setSel((s) => {
        const n = new Set(s)
        if (n.has(id)) n.delete(id)
        else n.add(id)
        return n
      }),
    [],
  )
  const clear = useCallback(() => setSel(new Set()), [])
  const setAll = useCallback((ids: string[], on: boolean) => setSel(new Set(on ? ids : [])), [])
  return { sel, toggle, clear, setAll }
}

/**
 * The bar at the bottom while rows are ticked: the actions they all offer. Each item runs its
 * own action through `triggerMany`, so one confirm lists every command and the server's
 * allow-list still checks each. Escape clears the selection.
 */
export function BulkBar({ items, noun, onClear }: { items: BulkItem[]; noun: [string, string]; onClear: () => void }) {
  const { triggerMany } = useAction()
  const n = items.length
  useEffect(() => {
    if (!n) return
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || document.querySelector("[role=dialog], [role=alertdialog], [role=menu]")) return
      const t = e.target as HTMLElement | null
      if (t instanceof HTMLInputElement && t.type !== "checkbox") return
      onClear()
    }
    window.addEventListener("keydown", onKey)
    return () => window.removeEventListener("keydown", onKey)
  }, [n, onClear])
  if (!n) return null
  const what = `${n} ${n === 1 ? noun[0] : noun[1]}`
  const kinds = commonKinds(items)
  return (
    <div
      role="region"
      aria-label="Selected rows"
      className="fixed inset-x-0 bottom-4 z-40 mx-auto flex w-fit max-w-[calc(100vw-32px)] flex-wrap items-center gap-1.5 rounded-xl border bg-popover px-3 py-2 text-sm shadow-[0_8px_32px_rgba(0,0,0,0.16)]"
    >
      <span className="px-1 font-medium tabular-nums">{what}</span>
      {kinds.length ? (
        kinds.map((a) => (
          <Button
            key={a.kind}
            size="sm"
            variant="outline"
            className={cn(DESTRUCTIVE.has(a.kind) && "border-alert-border text-alert-text hover:bg-alert-bg")}
            onClick={() => {
              triggerMany(
                items.map((i) => ({ a: i.actions.find((b) => b.kind === a.kind)!, subject: i.subject })),
                what,
                onClear,
              )
            }}
          >
            {short(a)}
          </Button>
        ))
      ) : (
        <span className="text-xs text-muted-foreground">No action they all offer</span>
      )}
      <Button size="icon-sm" variant="ghost" onClick={onClear} aria-label="Clear the selection" title="Clear (Esc)">
        <X />
      </Button>
    </div>
  )
}
