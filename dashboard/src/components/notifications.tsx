"use client"

import { useEffect, useRef, useState } from "react"
import { Bell, BellOff } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { DEFAULT_SETTINGS, fresh, NOTIFY_KINDS, type NotifyKind, type NotifySettings } from "@/lib/notify"
import type { State } from "@/lib/types"

const KEY = "rekcod.notify"
const SEEN = "rekcod.notified"

function load<T>(key: string, fallback: T): T {
  try {
    const v = localStorage.getItem(key)
    return v ? { ...fallback, ...JSON.parse(v) } : fallback
  } catch {
    return fallback
  }
}

function save(key: string, v: unknown) {
  try {
    localStorage.setItem(key, JSON.stringify(v))
  } catch {}
}

/**
 * Opt-in browser notifications. Everything already wrong when the page loads counts as seen,
 * and keys of shown events are kept in localStorage, so nothing is announced twice.
 */
export function useNotifications(state: State | null) {
  const [settings, setSettings] = useState<NotifySettings>(DEFAULT_SETTINGS)
  const seen = useRef<Set<string> | null>(null)

  useEffect(() => {
    // localStorage only exists in the browser; read it after mount.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setSettings(load(KEY, DEFAULT_SETTINGS))
  }, [])

  useEffect(() => {
    if (!state) return
    if (!seen.current) {
      seen.current = new Set(load<{ keys: string[] }>(SEEN, { keys: [] }).keys)
      fresh(state, seen.current, { ...settings, on: false })
      return
    }
    const list = fresh(state, seen.current, settings)
    if (list.length && typeof Notification !== "undefined" && Notification.permission === "granted") {
      for (const n of list) new Notification(n.title, { body: n.body, tag: n.key })
    }
    save(SEEN, { keys: [...seen.current].slice(-500) })
  }, [state, settings])

  const update = (s: NotifySettings) => {
    setSettings(s)
    save(KEY, s)
  }
  return { settings, update }
}

export function NotificationsButton({ state }: { state: State | null }) {
  const { settings, update } = useNotifications(state)
  const [open, setOpen] = useState(false)
  const [perm, setPerm] = useState<string>("default")
  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect
    if (typeof Notification !== "undefined") setPerm(Notification.permission)
  }, [open])

  async function enable(on: boolean) {
    if (on && typeof Notification !== "undefined" && Notification.permission !== "granted") {
      setPerm(await Notification.requestPermission())
    }
    update({ ...settings, on })
  }

  const Icon = settings.on && perm === "granted" ? Bell : BellOff
  return (
    <>
      <Button size="icon-sm" variant="ghost" onClick={() => setOpen(true)} aria-label="Notification settings" title="Notifications" className="text-muted-foreground">
        <Icon />
      </Button>
      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Notifications</DialogTitle>
            <DialogDescription>Shown by the browser while this page is open. Each event is announced once.</DialogDescription>
          </DialogHeader>
          {typeof Notification === "undefined" ? (
            <p className="text-sm text-muted-foreground">This browser has no notifications.</p>
          ) : (
            <div className="space-y-3 text-sm">
              <label className="flex items-center gap-2 font-medium">
                <input type="checkbox" checked={settings.on} onChange={(e) => enable(e.target.checked)} className="size-4 accent-foreground" />
                Notify me
              </label>
              {perm === "denied" && <p className="text-xs text-alert-text">The browser blocks notifications for this page; allow them in its site settings.</p>}
              <fieldset disabled={!settings.on} className="space-y-1.5 pl-6 disabled:opacity-50">
                <legend className="sr-only">Which events</legend>
                {(Object.keys(NOTIFY_KINDS) as NotifyKind[]).map((k) => (
                  <label key={k} className="flex items-center gap-2">
                    <input
                      type="checkbox"
                      checked={settings.kinds[k]}
                      onChange={(e) => update({ ...settings, kinds: { ...settings.kinds, [k]: e.target.checked } })}
                      className="size-4 accent-foreground"
                    />
                    {NOTIFY_KINDS[k]}
                  </label>
                ))}
              </fieldset>
            </div>
          )}
        </DialogContent>
      </Dialog>
    </>
  )
}
