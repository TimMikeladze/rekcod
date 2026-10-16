"use client"

import Link from "next/link"
import { usePathname } from "next/navigation"
import { Pin, PinOff } from "lucide-react"
import { containerTone, Dot, type Tone } from "@/components/bits"
import { Button } from "@/components/ui/button"
import { useDash } from "@/hooks/dash-context"
import { useLocal } from "@/hooks/use-local"
import { cn } from "@/lib/utils"
import type { State } from "@/lib/types"

/** What can be pinned: a container or an app, by name. */
export type PinRef = { kind: "containers" | "apps"; name: string }

const KEY = "rekcod.pins"
const same = (a: PinRef, b: PinRef) => a.kind === b.kind && a.name === b.name

export function usePins() {
  const [pins, setPins] = useLocal<PinRef[]>(KEY, [])
  const toggle = (p: PinRef) => setPins((list) => (list.some((x) => same(x, p)) ? list.filter((x) => !same(x, p)) : [...list, p]))
  return { pins, toggle, has: (p: PinRef) => pins.some((x) => same(x, p)) }
}

/** The pin toggle on a detail page's toolbar. */
export function PinButton({ pin }: { pin: PinRef }) {
  const { has, toggle } = usePins()
  const on = has(pin)
  return (
    <Button size="sm" variant="outline" aria-pressed={on} onClick={() => toggle(pin)} title={on ? "Unpin from the sidebar" : "Pin to the sidebar"}>
      {on ? <PinOff /> : <Pin />}
      <span className="max-sm:sr-only">{on ? "Unpin" : "Pin"}</span>
    </Button>
  )
}

/** A pin's state dot; a pin whose item is gone shows grey. */
function tone(s: State, p: PinRef): { tone: Tone; word: string } {
  if (p.kind === "containers") {
    const c = s.containers.find((x) => x.name === p.name)
    return c ? { tone: containerTone(c.state), word: c.state } : { tone: "idle", word: "gone" }
  }
  const a = s.apps.find((x) => x.name === p.name)
  if (!a) return { tone: "idle", word: "gone" }
  const lost = a.detail?.instances.some((i) => i.state === "lost")
  if (a.sleeping) return { tone: "idle", word: "asleep" }
  if (a.detail?.releases[0]?.state === "failed" || lost) return { tone: "bad", word: "problem" }
  return a.live_release == null ? { tone: "warn", word: "not live" } : { tone: "ok", word: "live" }
}

/** Pinned containers and apps, under Overview in the sidebar. Nothing when none are pinned. */
export function PinnedNav({ itemClass, labelClass }: { itemClass: string; labelClass: string }) {
  const { state } = useDash()
  const { pins } = usePins()
  const pathname = usePathname()
  if (!pins.length || !state) return null
  return (
    <div aria-label="Pinned" role="group" className="flex flex-col gap-0.5">
      {pins.map((p) => {
        const href = `/${p.kind}/${encodeURIComponent(p.name)}`
        const t = tone(state, p)
        return (
          <Link
            key={href}
            href={href}
            aria-current={pathname === href ? "page" : undefined}
            title={`${p.name} (${p.kind === "apps" ? "app" : "container"}, ${t.word})`}
            className={cn(itemClass, pathname === href && "bg-sidebar-accent font-medium text-foreground")}
          >
            <span className="inline-flex size-4 flex-none items-center justify-center">
              <Dot tone={t.tone} label={t.word} />
            </span>
            <span className={cn("truncate", labelClass)}>{p.name}</span>
            <span className={cn("ml-auto text-[11px] text-muted-foreground", labelClass)}>{p.kind === "apps" ? "app" : ""}</span>
          </Link>
        )
      })}
    </div>
  )
}
