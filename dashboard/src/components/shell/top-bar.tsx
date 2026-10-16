"use client"

import { useEffect, useMemo, useState } from "react"
import Link from "next/link"
import { usePathname, useRouter } from "next/navigation"
import { ChevronRight, RefreshCw } from "lucide-react"
import { ActivityButton } from "@/components/activity"
import { NotificationsButton } from "@/components/notifications"
import { sectionOf } from "@/components/shell/nav"
import { CollapseButton, DrawerButton, Mark } from "@/components/shell/sidebar"
import { Button } from "@/components/ui/button"
import { useDash } from "@/hooks/dash-context"
import { issues } from "@/lib/issues"

/** The 56 px bar on top of every page: where you are, then live/refresh, Activity and the bell. */
export function TopBar() {
  const { state, error, loadedAt, live, refresh } = useDash()
  const pathname = usePathname()
  const router = useRouter()
  const [now, setNow] = useState(0)
  useEffect(() => {
    const id = setInterval(() => setNow(Date.now()), 1000)
    return () => clearInterval(id)
  }, [])
  const age = now && loadedAt ? Math.max(0, Math.round((now - loadedAt) / 1000)) : 0
  const stale = !live && age > 15
  const section = sectionOf(pathname)
  // `/containers/web` reads "Containers › web"; `/stream` has no section.
  const item = pathname.split("/").slice(2).map(decodeURIComponent).join("/")
  // "(2) Apps · rekcod": the count of things that are broken (red in Needs attention).
  const problems = useMemo(() => (state ? issues(state).filter((i) => i.tone === "bad").length : 0), [state])
  const title = `${problems ? `(${problems}) ` : ""}${[item, section?.label ?? (pathname === "/stream" ? "Output" : ""), "rekcod"].filter(Boolean).join(" · ")}`
  // Pages render in the browser from live state, so the tab title is set here, not in metadata.
  useEffect(() => {
    document.title = title
  }, [title])
  return (
    <header className="sticky top-0 z-30 flex h-14 items-center justify-between gap-3 border-b bg-background px-5 max-md:px-3">
      <nav aria-label="Breadcrumb" className="flex min-w-0 items-center gap-1 text-sm">
        <DrawerButton />
        <CollapseButton />
        <Link href="/" aria-label="rekcod overview" className="mr-1.5 md:hidden">
          <Mark size={22} />
        </Link>
        {section ? (
          item ? (
            <>
              <Link href={section.href} className="text-muted-foreground hover:text-foreground">
                {section.label}
              </Link>
              <ChevronRight className="size-3.5 flex-none text-muted-foreground" aria-hidden />
              <span className="truncate font-medium" aria-current="page">
                {item}
              </span>
            </>
          ) : (
            <span className="font-medium" aria-current="page">
              {section.label}
            </span>
          )
        ) : (
          <span className="font-medium">{pathname === "/stream" ? "Output" : "rekcod"}</span>
        )}
      </nav>
      <div className="flex flex-none items-center gap-1">
        <Button
          size="sm"
          variant="ghost"
          onClick={refresh}
          className="text-xs text-muted-foreground tabular-nums"
          title={live ? "Live: docker events push changes; the fleet refreshes every 15 s" : "Refreshes every 3 s while this tab is visible"}
          aria-label={error ? "Refresh (last refresh failed)" : `Refresh (updated ${age} seconds ago)`}
        >
          {live && !error ? (
            <span className="relative inline-flex size-2">
              <span aria-hidden className="absolute inline-flex size-full animate-ping rounded-full bg-success opacity-60" />
              <span aria-hidden className="relative inline-flex size-2 rounded-full bg-success" />
            </span>
          ) : (
            <RefreshCw className={error || stale ? "text-alert-text" : ""} />
          )}
          <span className="max-sm:sr-only">{error ? "error" : live ? "live" : loadedAt ? `${age}s` : "…"}</span>
        </Button>
        {state && <ActivityButton onOpen={() => router.push("/activity")} />}
        <NotificationsButton state={state} />
      </div>
    </header>
  )
}
