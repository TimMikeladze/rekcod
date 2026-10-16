"use client"

import Link from "next/link"
import { usePathname, useSearchParams } from "next/navigation"
import { cn } from "@/lib/utils"

const seg = "inline-flex w-max gap-0.5 rounded-lg border bg-background p-[3px]"
const segItem =
  "inline-flex min-h-7 cursor-pointer items-center gap-1.5 rounded-[5px] px-2.5 text-[13px] font-medium whitespace-nowrap text-muted-foreground hover:text-foreground data-[on=true]:bg-accent data-[on=true]:text-foreground"

export type TabItem = { value: string; label: string; badge?: string | number }

/**
 * A page's own tabs as dux's TabRow: one segmented row of links that set `?<param>=`, keeping
 * the other parameters. It scrolls sideways on a phone instead of wrapping. The first item is
 * the default and drops the parameter.
 */
export function TabRow({ items, param = "tab", label, className }: { items: TabItem[]; param?: string; label: string; className?: string }) {
  const pathname = usePathname()
  const params = useSearchParams()
  const cur = useTab(items, param)
  return (
    <nav aria-label={label} className={cn("-mx-4 overflow-x-auto px-4 [scrollbar-width:none] md:mx-0 md:px-0 [&::-webkit-scrollbar]:hidden", className)}>
      <div className={seg}>
        {items.map((i, n) => {
          const p = new URLSearchParams(params.toString())
          if (n === 0) p.delete(param)
          else p.set(param, i.value)
          const qs = p.toString()
          const on = cur === i.value
          return (
            <Link key={i.value} href={qs ? `${pathname}?${qs}` : pathname} scroll={false} replace data-on={on} aria-current={on ? "page" : undefined} className={segItem}>
              {i.label}
              {i.badge != null && i.badge !== "" && <span className="font-mono text-[11px] text-muted-foreground tabular-nums">{i.badge}</span>}
            </Link>
          )
        })}
      </div>
    </nav>
  )
}

/** The selected tab's value: `?<param>=` when it names one of `items`, else the first. */
export function useTab(items: TabItem[], param = "tab"): string {
  const v = useSearchParams().get(param)
  return items.some((i) => i.value === v) ? v! : items[0].value
}
