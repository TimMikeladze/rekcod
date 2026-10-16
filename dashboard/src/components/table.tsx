"use client"

import { useEffect, useRef, useState, type KeyboardEvent, type ReactNode } from "react"
import { ArrowDown, ArrowUp, ChevronsUpDown } from "lucide-react"
import { TableHead, TableHeader, TableRow } from "@/components/ui/table"
import type { SortState } from "@/hooks/use-sort"
import { cn } from "@/lib/utils"
import type { Job } from "@/lib/types"

export const match = (q: string, ...fields: (string | null | undefined | number)[]) =>
  !q || fields.some((f) => f != null && String(f).toLowerCase().includes(q))

/** A column: its label, optional classes, and the `?sort=` key that sorts by it. */
export type Col = string | [string, string] | [string, string, string]

/** The header checkbox of a list with bulk select. */
export type SelectAll = { all: boolean; some: boolean; toggle: () => void }

export function Head({ cols, sort, select }: { cols: Col[]; sort?: SortState; select?: SelectAll }) {
  return (
    <TableHeader>
      <TableRow className="bg-subtle hover:bg-subtle">
        {select && (
          <TableHead className="hidden w-8 pr-0 sm:table-cell">
            <Check checked={select.all} indeterminate={select.some && !select.all} onChange={select.toggle} label="Select all rows" />
          </TableHead>
        )}
        {cols.map((c, i) => {
          const [label, cls, key] = Array.isArray(c) ? [c[0], c[1], c[2]] : [c, "", undefined]
          const on = !!key && sort?.key === key
          return (
            <TableHead
              key={label || `col${i}`}
              className={`text-xs ${cls}`}
              aria-sort={on ? (sort!.desc ? "descending" : "ascending") : undefined}
            >
              {!label ? (
                <span className="sr-only">Actions</span>
              ) : key && sort ? (
                <button
                  type="button"
                  onClick={() => sort.toggle(key)}
                  className={cn("-mx-1 inline-flex cursor-pointer items-center gap-1 rounded px-1 hover:text-foreground", on && "text-foreground")}
                  title={`Sort by ${label.toLowerCase()}`}
                >
                  {label}
                  {on ? sort.desc ? <ArrowDown className="size-3" /> : <ArrowUp className="size-3" /> : <ChevronsUpDown className="size-3 opacity-40 max-sm:hidden" />}
                </button>
              ) : (
                label
              )}
            </TableHead>
          )
        })}
      </TableRow>
    </TableHeader>
  )
}

/** A row or header checkbox. Clicks don't reach the row (which would open it). */
export function Check({ checked, indeterminate, onChange, label }: { checked: boolean; indeterminate?: boolean; onChange: () => void; label: string }) {
  const ref = useRef<HTMLInputElement>(null)
  useEffect(() => {
    if (ref.current) ref.current.indeterminate = !!indeterminate
  }, [indeterminate])
  return (
    <input
      ref={ref}
      type="checkbox"
      data-row-check
      checked={checked}
      onChange={onChange}
      onClick={(e) => e.stopPropagation()}
      aria-label={label}
      className="size-3.5 cursor-pointer align-middle accent-foreground"
    />
  )
}

/** Moves focus to the previous/next `[data-navrow]` in the same table. */
function step(from: HTMLElement, by: number) {
  const rows = Array.from(from.closest("table")?.querySelectorAll<HTMLElement>("[data-navrow]") ?? [])
  const i = rows.indexOf(from)
  rows[Math.max(0, Math.min(rows.length - 1, i + by))]?.focus()
}

/** True for 1.5 s after `value` changes (not on mount). */
export function useFlash(value: string | undefined): boolean {
  const prev = useRef(value)
  const [on, setOn] = useState(false)
  useEffect(() => {
    if (prev.current === value) return
    const was = prev.current
    prev.current = value
    if (was === undefined) return
    setOn(true)
    const t = setTimeout(() => setOn(false), 1500)
    return () => clearTimeout(t)
  }, [value])
  return on
}

/**
 * A row you can reach with the keyboard: Tab or arrow keys move between rows, Enter (or a
 * click) opens it, `.` opens its actions menu, `x` ticks its checkbox. `label` is what a
 * screen reader announces. When `watch` changes (a container stopped, a release went live)
 * the row flashes.
 */
export function NavRow({
  onOpen,
  label,
  className,
  watch,
  selected,
  children,
}: {
  onOpen?: () => void
  label: string
  className?: string
  watch?: string
  selected?: boolean
  children: ReactNode
}) {
  const flash = useFlash(watch)
  function onKeyDown(e: KeyboardEvent<HTMLTableRowElement>) {
    if (e.target !== e.currentTarget) return
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault()
      step(e.currentTarget, e.key === "ArrowDown" ? 1 : -1)
    } else if (e.key === "Enter" && onOpen) {
      e.preventDefault()
      onOpen()
    } else if (e.key === ".") {
      e.preventDefault()
      e.currentTarget.querySelector<HTMLElement>("[data-row-menu]")?.click()
    } else if (e.key === "x") {
      const box = e.currentTarget.querySelector<HTMLElement>("[data-row-check]")
      if (box) {
        e.preventDefault()
        box.click()
      }
    }
  }
  return (
    <TableRow
      data-navrow
      data-flash={flash || undefined}
      data-state={selected ? "selected" : undefined}
      tabIndex={0}
      aria-label={label}
      onKeyDown={onKeyDown}
      onClick={onOpen}
      className={cn("outline-none focus-visible:bg-subtle focus-visible:ring-2 focus-visible:ring-ring/50 focus-visible:ring-inset", onOpen && "cursor-pointer", className)}
    >
      {children}
    </TableRow>
  )
}

export function SectionTitle({ children, right }: { children: ReactNode; right?: ReactNode }) {
  return (
    <div className="mb-3 flex min-h-7 items-center gap-2">
      <h2 className="h2">{children}</h2>
      {right && <div className="ml-auto">{right}</div>}
    </div>
  )
}

export function cmdLine(cmd: Job["cmd"]): string {
  if (!cmd) return ""
  return Array.isArray(cmd) ? cmd.join(" ") : String(cmd)
}
