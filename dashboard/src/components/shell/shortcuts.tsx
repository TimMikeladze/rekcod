"use client"

import { useEffect, useRef, useState } from "react"
import { useRouter } from "next/navigation"
import { PAGES, SECTIONS } from "@/components/shell/nav"
import { Dialog, DialogContent, DialogTitle } from "@/components/ui/dialog"

/** Typing in a field, or a menu/dialog is open: page keys stay out of the way. */
function busy(e: KeyboardEvent) {
  const t = e.target as HTMLElement | null
  if (t instanceof HTMLInputElement || t instanceof HTMLTextAreaElement || t instanceof HTMLSelectElement || t?.isContentEditable) return true
  return !!document.querySelector("[role=dialog], [role=alertdialog], [role=menu]")
}

/** How long after `g` the second key still counts. */
const CHORD_MS = 1200

const GENERAL: [string, string][] = [
  ["⌘K", "Find rows and actions"],
  ["⌘B", "Fold the sidebar"],
  ["?", "This list"],
  ["/", "Filter the list"],
  ["1-9", "List pages, in sidebar order"],
]

const ROWS: [string, string][] = [
  ["↑ ↓", "Move between rows"],
  ["↵", "Open the row"],
  [".", "The row's actions menu"],
  ["x", "Select the row (containers, images, volumes)"],
  ["esc", "Clear the selection or the filter"],
]

const LOGS: [string, string][] = [
  ["↵ / ⇧↵", "Next / previous match in the log search"],
]

function Keys({ title, rows }: { title: string; rows: [string, string][] }) {
  return (
    <section>
      <h3 className="cap mb-1.5">{title}</h3>
      <dl className="grid grid-cols-[5.5rem_1fr] gap-y-1.5 text-[13px]">
        {rows.map(([k, v]) => (
          <div key={k} className="contents">
            <dt>
              <span className="kbd">{k}</span>
            </dt>
            <dd>{v}</dd>
          </div>
        ))}
      </dl>
    </section>
  )
}

/**
 * Page keys: `?` opens the sheet, `g` then a page's letter goes there, 1-9 open the list pages.
 * None of them fire while typing or with a menu or dialog open.
 */
export function Shortcuts() {
  const router = useRouter()
  const [open, setOpen] = useState(false)
  const g = useRef(0)
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.metaKey || e.ctrlKey || e.altKey || busy(e)) return
      if (e.key === "?") {
        e.preventDefault()
        setOpen(true)
        return
      }
      if (Date.now() - g.current < CHORD_MS) {
        g.current = 0
        const page = PAGES.find((p) => p.go === e.key)
        if (page) {
          e.preventDefault()
          router.push(page.href)
        }
        return
      }
      if (e.key === "g") {
        g.current = Date.now()
        return
      }
      if (/^[1-9]$/.test(e.key)) router.push(SECTIONS[Number(e.key) - 1].href)
    }
    window.addEventListener("keydown", onKey)
    return () => window.removeEventListener("keydown", onKey)
  }, [router])
  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogContent className="max-h-[85vh] overflow-y-auto sm:max-w-[560px]">
        <DialogTitle>Keyboard shortcuts</DialogTitle>
        <div className="grid gap-6 sm:grid-cols-2">
          <div className="space-y-5">
            <Keys title="Anywhere" rows={GENERAL} />
            <Keys title="Rows" rows={ROWS} />
            <Keys title="Logs" rows={LOGS} />
          </div>
          <Keys title="Go to (g, then)" rows={PAGES.map((p) => [`g ${p.go}`, p.label])} />
        </div>
      </DialogContent>
    </Dialog>
  )
}
