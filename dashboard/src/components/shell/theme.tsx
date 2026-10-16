"use client"

import { Monitor, Moon, Rows3, Rows4, Sun } from "lucide-react"
import { ThemeProvider as NextThemes, useTheme } from "next-themes"
import { useEffect, useState } from "react"
import { cn } from "@/lib/utils"

/** System by default; `.dark` on <html> drives every token in globals.css. The pick is kept in localStorage. */
export function ThemeProvider({ children }: { children: React.ReactNode }) {
  return (
    <NextThemes attribute="class" defaultTheme="system" enableSystem disableTransitionOnChange>
      {children}
    </NextThemes>
  )
}

const OPTIONS = [
  { value: "system", label: "System", Icon: Monitor },
  { value: "light", label: "Light", Icon: Sun },
  { value: "dark", label: "Dark", Icon: Moon },
] as const

/** "Theme" row at the bottom of the sidebar: a three-way segmented control. */
export function ThemeSwitch() {
  const { theme, setTheme } = useTheme()
  // The stored pick is only known in the browser; render "system" until mounted.
  const [mounted, setMounted] = useState(false)
  // eslint-disable-next-line react-hooks/set-state-in-effect
  useEffect(() => setMounted(true), [])
  const current = mounted ? (theme ?? "system") : "system"
  return (
    <div className="flex items-center justify-between gap-2 px-2.5 py-1 text-sm text-sidebar-foreground">
      <span>Theme</span>
      <div role="radiogroup" aria-label="Theme" className="flex items-center gap-0.5 rounded-full border p-0.5">
        {OPTIONS.map(({ value, label, Icon }) => (
          <button
            key={value}
            type="button"
            role="radio"
            aria-checked={current === value}
            aria-label={label}
            title={label}
            onClick={() => setTheme(value)}
            className={cn(
              "inline-flex size-6 cursor-pointer items-center justify-center rounded-full text-muted-foreground hover:text-foreground",
              current === value && "bg-accent text-foreground",
            )}
          >
            <Icon className="size-3.5" />
          </button>
        ))}
      </div>
    </div>
  )
}

export const DENSITY_KEY = "rekcod.density"

function applyDensity(compact: boolean) {
  const d = document.documentElement.dataset
  if (compact) d.density = "compact"
  else delete d.density
  try {
    localStorage.setItem(DENSITY_KEY, compact ? "compact" : "comfortable")
  } catch {}
}

/** "Tables" row under Theme: comfortable or compact rows, on `<html data-density>` (set before paint). */
export function DensitySwitch() {
  const [compact, setCompact] = useState(false)
  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setCompact(document.documentElement.dataset.density === "compact")
  }, [])
  const pick = (c: boolean) => {
    applyDensity(c)
    setCompact(c)
  }
  return (
    <div className="flex items-center justify-between gap-2 px-2.5 py-1 text-sm text-sidebar-foreground">
      <span>Tables</span>
      <div role="radiogroup" aria-label="Table density" className="flex items-center gap-0.5 rounded-full border p-0.5">
        {([false, true] as const).map((c) => (
          <button
            key={String(c)}
            type="button"
            role="radio"
            aria-checked={compact === c}
            aria-label={c ? "Compact" : "Comfortable"}
            title={c ? "Compact" : "Comfortable"}
            onClick={() => pick(c)}
            className={cn(
              "inline-flex size-6 cursor-pointer items-center justify-center rounded-full text-muted-foreground hover:text-foreground",
              compact === c && "bg-accent text-foreground",
            )}
          >
            {c ? <Rows4 className="size-3.5" /> : <Rows3 className="size-3.5" />}
          </button>
        ))}
      </div>
    </div>
  )
}
