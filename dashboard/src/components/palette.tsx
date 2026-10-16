"use client"

import { useEffect, useMemo, useRef, useState } from "react"
import { useRouter } from "next/navigation"
import { CornerDownRight, File, Play, Search } from "lucide-react"
import { short, useAction } from "@/components/actions"
import { Mono } from "@/components/bits"
import { PAGES } from "@/components/shell/nav"
import { Dialog, DialogContent, DialogTitle } from "@/components/ui/dialog"
import { useLocal } from "@/hooks/use-local"
import type { Tab } from "@/lib/issues"
import type { Action, State } from "@/lib/types"
import { cn } from "@/lib/utils"

export type Entry =
  | { type: "action"; text: string; hint: string; action: Action; subject: string }
  | { type: "go"; text: string; hint: string; href: string }
  | { type: "page"; text: string; hint: string; href: string }

/** What the recents list remembers an entry by. */
export const entryId = (e: Entry) => (e.type === "action" ? `a:${JSON.stringify(e.action.argv ?? e.action.url)}` : `${e.type}:${e.href}`)

const RECENT_KEY = "rekcod.palette.recent"
const RECENT_MAX = 8

const enc = encodeURIComponent

/** Every page, every row worth jumping to (its detail page, or its list filtered to it) and every action the state offers. */
export function entries(s: State): Entry[] {
  const out: Entry[] = PAGES.map((p) => ({ type: "page", text: p.label, hint: `page · g ${p.go}`, href: p.href }))
  const acts = (list: Action[] | undefined, subject: string) => {
    for (const a of list ?? []) {
      if (a.kind === "DashboardOpen") continue
      const verb = short(a)
      const text = verb.toLowerCase().includes(subject.toLowerCase()) ? verb : `${verb} ${subject}`
      out.push({ type: "action", text, hint: a.argv ? `rekcod ${a.argv.join(" ")}` : (a.url ?? ""), action: a, subject })
    }
  }
  const go = (text: string, hint: string, href: string) => out.push({ type: "go", text, hint, href })
  const list = (tab: Tab, filter: string) => `/${tab}?q=${enc(filter)}`
  for (const c of s.containers) {
    go(c.name, `container · ${c.image}`, `/containers/${enc(c.name)}`)
    acts(c.actions, c.name)
  }
  for (const p of s.projects) {
    go(p.name, "compose project", list("projects", p.name))
    acts(p.actions, p.name)
    for (const sn of p.snapshots) {
      go(`${p.name}/${sn.name}`, "snapshot", list("projects", sn.name))
      acts(sn.actions, `${p.name}/${sn.name}`)
    }
  }
  for (const i of s.images ?? []) {
    const name = i.tags[0] ?? i.id.replace("sha256:", "").slice(0, 12)
    go(name, "image", list("images", name))
    acts(i.actions, name)
  }
  for (const v of s.volumes ?? []) {
    go(v.name, "volume", list("volumes", v.name))
    acts(v.actions, v.name)
  }
  for (const p of s.ports) {
    go(`:${p.port}`, `port · ${p.container}`, list("ports", String(p.port)))
    acts(p.actions, `:${p.port}`)
  }
  for (const a of s.apps) {
    go(a.name, `app · ${a.url}`, `/apps/${enc(a.name)}`)
    acts(a.actions, a.name)
  }
  for (const j of s.jobs) {
    const name = j.name ?? j.id
    go(name, `job · ${j.state}`, `/jobs/${enc(j.id)}`)
    acts(j.actions, name)
  }
  for (const sb of s.sandboxes) {
    const name = sb.name ?? sb.id
    go(name, "agent sandbox", `/sandboxes/${enc(sb.id)}`)
    acts(sb.actions, name)
  }
  for (const n of s.nodes ?? []) {
    go(n.id, `node · ${n.state}`, `/nodes/${enc(n.id)}`)
    acts(n.actions, n.id)
  }
  acts(s.vm_actions, "rekcod")
  acts(s.storage_actions, "S3 storage")
  acts(s.disk_actions, "disk")
  acts(s.hosts.actions, "docker host")
  acts(s.namespaces.actions, "namespace")
  acts(s.bans?.actions, "edge")
  acts(s.remotes?.actions, "remote")
  acts(s.controller_actions, "controller")
  return out
}

/**
 * Entries whose text or hint contains every word of `q`: recently picked ones first, then ones
 * whose text starts with `q`. An empty `q` lists the recent ones, then the pages.
 */
export function search(list: Entry[], q: string, recent: string[] = [], max = 50): Entry[] {
  const rank = (e: Entry) => {
    const i = recent.indexOf(entryId(e))
    return i < 0 ? recent.length : i
  }
  const words = q.toLowerCase().split(/\s+/).filter(Boolean)
  if (!words.length) {
    const seen = list.filter((e) => recent.includes(entryId(e))).sort((a, b) => rank(a) - rank(b))
    return [...seen, ...list.filter((e) => e.type === "page" && !seen.includes(e))].slice(0, max)
  }
  const hits = list.filter((e) => {
    const t = `${e.text} ${e.hint}`.toLowerCase()
    return words.every((w) => t.includes(w))
  })
  const head = q.toLowerCase()
  const starts = (e: Entry) => Number(!e.text.toLowerCase().startsWith(head))
  return hits.sort((a, b) => rank(a) - rank(b) || starts(a) - starts(b)).slice(0, max)
}

/** `id` first in the recents list, at most RECENT_MAX long. */
export const remember = (recent: string[], id: string) => [id, ...recent.filter((r) => r !== id)].slice(0, RECENT_MAX)

/**
 * Cmd+K: jump to any row or run any offered action ("restart web", "logs dash", "wake blog").
 * Actions go through the same `trigger` as the buttons, so confirms and the allow-list apply.
 */
export function Palette({ state }: { state: State }) {
  const [open, setOpen] = useState(false)
  const [q, setQ] = useState("")
  const [sel, setSel] = useState(0)
  const { trigger } = useAction()
  const router = useRouter()
  const [recent, setRecent] = useLocal<string[]>(RECENT_KEY, [])
  const list = useRef<HTMLDivElement>(null)
  const all = useMemo(() => entries(state), [state])
  const shown = useMemo(() => search(all, q, recent), [all, q, recent])

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key.toLowerCase() === "k" && (e.metaKey || e.ctrlKey)) {
        e.preventDefault()
        setOpen((o) => !o)
        setQ("")
        setSel(0)
      }
    }
    window.addEventListener("keydown", onKey)
    return () => window.removeEventListener("keydown", onKey)
  }, [])

  useEffect(() => {
    list.current?.querySelector(`[data-i="${sel}"]`)?.scrollIntoView({ block: "nearest" })
  }, [sel])

  function pick(e: Entry | undefined) {
    if (!e) return
    setOpen(false)
    setRecent((r) => remember(r, entryId(e)))
    if (e.type === "action") trigger(e.action, e.subject)
    else router.push(e.href)
  }

  // Recent first (when there's no query), then pages, rows, actions, in search order within
  // each; `sel` indexes this order.
  const recentSet = new Set(q.trim() ? [] : recent)
  const isRecent = (e: Entry) => recentSet.has(entryId(e))
  const groups: { name: string; items: Entry[] }[] = [
    { name: "Recent", items: shown.filter(isRecent) },
    { name: "Pages", items: shown.filter((e) => e.type === "page" && !isRecent(e)) },
    { name: "Go to", items: shown.filter((e) => e.type === "go" && !isRecent(e)) },
    { name: "Run", items: shown.filter((e) => e.type === "action" && !isRecent(e)) },
  ].filter((g) => g.items.length)
  const flat = groups.flatMap((g) => g.items)

  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogContent showCloseButton={false} className="top-[12vh] max-w-[640px] translate-y-0 gap-0 overflow-hidden rounded-xl p-0 shadow-[0_16px_48px_rgba(0,0,0,0.14)] sm:max-w-[640px]">
        <DialogTitle className="sr-only">Command palette</DialogTitle>
        <div className="flex items-center gap-2.5 border-b border-hairline px-4">
          <Search className="size-4 text-muted-foreground" aria-hidden />
          <input
            autoFocus
            value={q}
            onChange={(e) => {
              setQ(e.target.value)
              setSel(0)
            }}
            onKeyDown={(e) => {
              if (e.key === "ArrowDown") {
                e.preventDefault()
                setSel((i) => Math.min(flat.length - 1, i + 1))
              } else if (e.key === "ArrowUp") {
                e.preventDefault()
                setSel((i) => Math.max(0, i - 1))
              } else if (e.key === "Enter") {
                e.preventDefault()
                pick(flat[sel])
              }
            }}
            placeholder="Restart web, logs dash, disk…"
            aria-label="Search rows and actions"
            aria-controls="palette-list"
            aria-activedescendant={flat[sel] ? `palette-${sel}` : undefined}
            role="combobox"
            aria-expanded
            className="h-[52px] min-w-0 flex-1 bg-transparent text-[15px] outline-none placeholder:text-muted-foreground focus-visible:outline-none"
          />
          <span className="kbd max-sm:hidden">esc</span>
        </div>
        <div ref={list} id="palette-list" role="listbox" aria-label="Results" className="h-[min(380px,55vh)] overflow-y-auto p-2">
          {!flat.length && <div className="px-3 py-10 text-center text-sm text-muted-foreground">Nothing matches.</div>}
          {groups.map((g) => (
            <div key={g.name} role="group" aria-label={g.name}>
              <div className="px-3 pt-2.5 pb-1 text-xs text-muted-foreground">{g.name}</div>
              {g.items.map((e) => {
                const i = flat.indexOf(e)
                const danger = e.type === "action" && !!e.action.confirm
                return (
                  <div
                    key={`${e.type}:${e.text}:${e.hint}`}
                    id={`palette-${i}`}
                    data-i={i}
                    role="option"
                    aria-selected={i === sel}
                    onMouseMove={() => setSel(i)}
                    onClick={() => pick(e)}
                    className={cn("flex min-h-11 cursor-pointer items-center gap-3 rounded-lg px-3 text-sm", i === sel && "bg-accent")}
                  >
                    <span
                      aria-hidden
                      className={cn(
                        "inline-flex size-[26px] flex-none items-center justify-center rounded-md border text-code",
                        danger && "border-alert-border text-alert-text",
                      )}
                    >
                      {e.type === "page" ? <File className="size-3.5" /> : e.type === "go" ? <CornerDownRight className="size-3.5" /> : <Play className="size-3" />}
                    </span>
                    <span className="min-w-0 flex-1">
                      <span className={cn("block truncate", danger && "text-alert-text")}>{e.text}</span>
                      <Mono className="block truncate text-muted-foreground">{e.hint}</Mono>
                      {danger && <span className="block truncate text-xs text-alert-text">{e.action.confirm}</span>}
                    </span>
                    {i === sel && <span className="kbd">↵</span>}
                  </div>
                )
              })}
            </div>
          ))}
        </div>
        <div className="flex gap-4 border-t border-hairline px-4 py-2.5 text-xs text-muted-foreground max-sm:hidden">
          <span>
            <span className="kbd">↑↓</span> move
          </span>
          <span>
            <span className="kbd">↵</span> open or run
          </span>
          <span>
            <span className="kbd">esc</span> close
          </span>
        </div>
      </DialogContent>
    </Dialog>
  )
}
