"use client"

import { createContext, useCallback, useContext, useEffect, useMemo, useState } from "react"
import Link from "next/link"
import { usePathname } from "next/navigation"
import { ChevronsUpDown, Menu, PanelLeft, Search, X } from "lucide-react"
import { useAction } from "@/components/actions"
import { Dot } from "@/components/bits"
import { counts, OVERVIEW, SECTIONS, sectionOf, SYSTEM, type NavItem } from "@/components/shell/nav"
import { PinnedNav } from "@/components/shell/pins"
import { DensitySwitch, ThemeSwitch } from "@/components/shell/theme"
import { Button } from "@/components/ui/button"
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"
import { useDash } from "@/hooks/dash-context"
import { fmtBytes } from "@/lib/format"
import { issues, type Tab } from "@/lib/issues"
import { cn } from "@/lib/utils"
import type { Action, State as S } from "@/lib/types"

const navClass =
  "flex min-h-9 items-center gap-2.5 rounded-md px-2.5 text-sm text-sidebar-foreground hover:bg-accent hover:text-foreground [&_svg]:size-4 [&_svg]:flex-none rail:justify-center rail:px-0"
const groupClass = "px-2.5 pt-4 pb-1 text-[11px] font-medium tracking-[0.04em] text-muted-foreground uppercase whitespace-nowrap rail:mx-2 rail:my-2.5 rail:h-px rail:overflow-hidden rail:bg-border rail:p-0 rail:text-transparent"
/** Hidden while the sidebar is an icon rail. */
const label = "rail:hidden"

/** The rekcod mark: a rounded square with a whale-less container. */
export function Mark({ size = 24 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" aria-hidden="true" className="flex-none">
      <rect width="24" height="24" rx="6" fill="var(--foreground)" />
      <path d="M6 9.5h12v7H6zM9 9.5v7M12 9.5v7M15 9.5v7M8.5 6.5h7v3h-7z" fill="none" stroke="var(--background)" strokeWidth="1.5" strokeLinejoin="round" />
    </svg>
  )
}

type Drawer = { open: boolean; setOpen: (o: boolean) => void; collapsed: boolean; toggleCollapsed: () => void }
const DrawerCtx = createContext<Drawer>({ open: false, setOpen: () => {}, collapsed: false, toggleCollapsed: () => {} })
export const SIDEBAR_KEY = "rekcod.sidebar.collapsed"

/**
 * The sidebar's two states: the phone drawer (open or not) and, from `md` up, the icon rail
 * (collapsed or not, Cmd+B, kept in localStorage and put on `<html data-sidebar>` before paint). The top bar's buttons and the sidebar share them.
 */
export function DrawerProvider({ children }: { children: React.ReactNode }) {
  const pathname = usePathname()
  // Open on the page it was opened on: navigating closes it.
  const [openOn, setOpenOn] = useState<string | null>(null)
  // The layout's head script already set the attribute; this mirrors it for aria-pressed.
  const [collapsed, setCollapsed] = useState(false)
  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setCollapsed(document.documentElement.dataset.sidebar === "collapsed")
  }, [])
  const toggleCollapsed = useCallback(() => {
    const next = document.documentElement.dataset.sidebar !== "collapsed"
    if (next) document.documentElement.dataset.sidebar = "collapsed"
    else delete document.documentElement.dataset.sidebar
    try {
      localStorage.setItem(SIDEBAR_KEY, next ? "1" : "0")
    } catch {}
    setCollapsed(next)
  }, [])
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key.toLowerCase() === "b" && (e.metaKey || e.ctrlKey) && !e.altKey && !e.shiftKey) {
        e.preventDefault()
        toggleCollapsed()
      }
    }
    window.addEventListener("keydown", onKey)
    return () => window.removeEventListener("keydown", onKey)
  }, [toggleCollapsed])
  const value = useMemo(
    () => ({ open: openOn === pathname, setOpen: (o: boolean) => setOpenOn(o ? pathname : null), collapsed, toggleCollapsed }),
    [openOn, pathname, collapsed, toggleCollapsed],
  )
  return <DrawerCtx.Provider value={value}>{children}</DrawerCtx.Provider>
}

/** The button that folds the sidebar into an icon rail (from `md` up). */
export function CollapseButton() {
  const { collapsed, toggleCollapsed } = useContext(DrawerCtx)
  return (
    <Button
      variant="ghost"
      size="icon-sm"
      aria-label={collapsed ? "Expand sidebar" : "Collapse sidebar"}
      aria-pressed={collapsed}
      title={`${collapsed ? "Expand" : "Collapse"} sidebar (⌘B)`}
      className="-ml-2 mr-1 text-muted-foreground max-md:hidden"
      onClick={toggleCollapsed}
    >
      <PanelLeft />
    </Button>
  )
}

/** The menu button that opens the sidebar below `md`. */
export function DrawerButton() {
  const { open, setOpen } = useContext(DrawerCtx)
  return (
    <Button variant="ghost" size="icon" aria-label="Open navigation" aria-expanded={open} aria-controls="sidebar" className="-ml-1.5 md:hidden" onClick={() => setOpen(true)}>
      <Menu />
    </Button>
  )
}

/** The sidebar. From `md` up it's a sticky column; below, a drawer over the page. One tree for both. */
export function Sidebar() {
  const { open, setOpen } = useContext(DrawerCtx)
  useEffect(() => {
    if (!open) return
    const onKey = (e: KeyboardEvent) => {
      // Escape closes an open menu or dialog first, the drawer only when nothing is layered on it.
      if (e.key === "Escape" && !document.querySelector("[role=menu], [role=dialog], [role=alertdialog]")) setOpen(false)
    }
    window.addEventListener("keydown", onKey)
    const overflow = document.body.style.overflow
    document.body.style.overflow = "hidden"
    return () => {
      window.removeEventListener("keydown", onKey)
      document.body.style.overflow = overflow
    }
    // setOpen changes with the path; the drawer is already closed by then.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open])

  return (
    <>
      {open && <div aria-hidden="true" className="fixed inset-0 z-40 bg-black/40 md:hidden" onClick={() => setOpen(false)} />}
      <aside
        id="sidebar"
        aria-label="Sidebar"
        data-open={open}
        onClick={(e) => {
          if ((e.target as HTMLElement).closest("a")) setOpen(false)
        }}
        className="flex flex-col gap-0.5 overflow-y-auto border-r bg-sidebar p-2.5 max-md:invisible max-md:fixed max-md:inset-y-0 max-md:left-0 max-md:z-50 max-md:w-[min(86vw,300px)] max-md:-translate-x-full max-md:transition-[transform,visibility] max-md:duration-200 max-md:data-[open=true]:visible max-md:data-[open=true]:translate-x-0 md:sticky md:top-0 md:h-screen md:w-[248px] md:flex-none md:transition-[width] md:duration-200 rail:w-[60px] rail:overflow-x-hidden"
      >
        <Button variant="ghost" size="icon" aria-label="Close navigation" className="self-end md:hidden" onClick={() => setOpen(false)}>
          <X />
        </Button>
        <SidebarBody />
      </aside>
    </>
  )
}

function SidebarBody() {
  const { state } = useDash()
  const pathname = usePathname()
  const active = sectionOf(pathname)?.href
  const n = useMemo(() => (state ? counts(state) : null), [state])
  // A red badge on a list with something wrong in it; otherwise its count.
  const bad = useMemo(() => {
    const by: Partial<Record<Tab, number>> = {}
    for (const i of state ? issues(state) : []) if (i.tone === "bad") by[i.tab] = (by[i.tab] ?? 0) + 1
    return by
  }, [state])
  const item = (it: NavItem) => (
    <NavLink key={it.href} item={it} on={active === it.href} count={it.tab ? n?.[it.tab] : undefined} alerts={it.tab ? bad[it.tab] : undefined} />
  )
  return (
    <>
      {state ? <VmSwitcher state={state} /> : <div className="h-11" />}
      <button
        type="button"
        onClick={() => window.dispatchEvent(new KeyboardEvent("keydown", { key: "k", metaKey: true }))}
        aria-label="Find rows and actions"
        title="Find (⌘K)"
        className="mt-1.5 mb-2 inline-flex min-h-9 w-full cursor-pointer items-center justify-between rounded-md border bg-background px-3 text-sm text-muted-foreground hover:border-strong rail:justify-center rail:px-0"
      >
        <span className="inline-flex items-center gap-2">
          <Search className="size-[15px] flex-none" />
          <span className={label}>Find…</span>
        </span>
        <span className={cn("kbd", label)}>⌘K</span>
      </button>
      <nav aria-label="Main" className="flex flex-col gap-0.5">
        {item(OVERVIEW)}
        <PinnedNav itemClass={navClass} labelClass={label} />
        <div className={groupClass}>This Mac</div>
        {SECTIONS.filter((s) => s.group === "This Mac").map(item)}
        <div className={groupClass}>Fleet</div>
        {SECTIONS.filter((s) => s.group === "Fleet").map(item)}
        <div className={groupClass}>System</div>
        {SYSTEM.map(item)}
      </nav>
      <div className="mt-auto flex flex-col gap-2 pt-6 rail:hidden">
        {state && <RemoteCard state={state} />}
        <ThemeSwitch />
        <DensitySwitch />
      </div>
    </>
  )
}

function NavLink({ item, on, count, alerts }: { item: NavItem; on: boolean; count?: string; alerts?: number }) {
  const Icon = item.icon
  const shortcut = item.tab ? SECTIONS.findIndex((s) => s.tab === item.tab) + 1 : 0
  return (
    <Link
      href={item.href}
      aria-current={on ? "page" : undefined}
      title={shortcut ? `${item.label} (press ${shortcut})` : item.label}
      className={cn(navClass, "relative", on && "bg-sidebar-accent font-medium text-foreground")}
    >
      <Icon />
      <span className={cn("truncate", label)}>{item.label}</span>
      {alerts ? (
        <span aria-hidden className="absolute top-1.5 left-6 hidden size-2 rounded-full bg-alert ring-2 ring-sidebar rail:block" />
      ) : null}
      {alerts ? (
        <span
          className="ml-auto rail:hidden inline-flex h-[18px] min-w-[18px] items-center justify-center rounded-full bg-alert px-1 text-[11px] font-semibold text-white tabular-nums"
          aria-label={`${alerts} problem${alerts > 1 ? "s" : ""}`}
        >
          {alerts}
        </span>
      ) : count ? (
        <span className="ml-auto font-mono text-xs text-muted-foreground tabular-nums rail:hidden">{count}</span>
      ) : null}
    </Link>
  )
}

function ActionItems({ actions, subject }: { actions: Action[]; subject: string }) {
  const { trigger } = useAction()
  return actions.map((a) => (
    <DropdownMenuItem key={a.kind + a.label} onClick={() => trigger(a, subject)} variant={a.kind === "VmStop" || a.kind === "VmForceStop" ? "destructive" : "default"}>
      {a.label}
    </DropdownMenuItem>
  ))
}

/** The top of the sidebar: the mark, the VM's state, and a menu with the VM and storage actions. */
function VmSwitcher({ state }: { state: S }) {
  const st = state.status
  const vm = state.vm_actions.filter((a) => a.kind !== "DashboardOpen")
  return (
    <DropdownMenu>
      <DropdownMenuTrigger
        render={<Button variant="ghost" className="h-11 w-full justify-between px-2 rail:justify-center rail:px-0" aria-label={`rekcod VM, ${state.running ? "running" : "stopped"}`} />}
      >
        <span className="inline-flex min-w-0 items-center gap-2.5">
          <Mark />
          <span className={cn("font-semibold", label)}>rekcod</span>
          <span
            className={cn(
              "inline-flex items-center gap-1 rounded-full px-[7px] py-px text-[11px] font-medium",
              label,
              state.running ? "bg-primary text-primary-foreground" : "border text-muted-foreground",
            )}
          >
            {state.running ? "running" : "stopped"}
          </span>
        </span>
        <ChevronsUpDown className={cn("size-3.5 text-muted-foreground", label)} />
      </DropdownMenuTrigger>
      <DropdownMenuContent align="start" sideOffset={6} className="w-64">
        <DropdownMenuGroup>
          <DropdownMenuLabel className="font-mono text-[11px]">
            {state.running ? `dockerd ${st.dockerd ?? "?"} · ${st.cpus ?? "?"} CPU · ${fmtBytes((st.memory_mib ?? 0) * 1024 * 1024)}` : "The VM is stopped"}
          </DropdownMenuLabel>
        </DropdownMenuGroup>
        <DropdownMenuItem render={<Link href="/vm" />}>VM details</DropdownMenuItem>
        <DropdownMenuSeparator />
        <ActionItems actions={vm.filter((a) => a.kind !== "VmStop" && a.kind !== "VmForceStop")} subject="rekcod" />
        {state.storage_actions.length > 0 && <DropdownMenuSeparator />}
        <ActionItems actions={state.storage_actions} subject="S3 storage" />
        {vm.some((a) => a.kind === "VmStop") && <DropdownMenuSeparator />}
        <ActionItems actions={vm.filter((a) => a.kind === "VmStop" || a.kind === "VmForceStop")} subject="rekcod" />
      </DropdownMenuContent>
    </DropdownMenu>
  )
}

/** The bottom of the sidebar: the remote and namespace the fleet pages show, and the docker host. */
function RemoteCard({ state }: { state: S }) {
  const host = state.hosts.local_current ? "local" : (state.hosts.all.find((h) => h.current)?.name ?? state.status.context ?? "?")
  const remoteActions = [...state.namespaces.actions, ...(state.remotes?.actions ?? []), ...(state.controller_actions ?? [])]
  return (
    <section aria-label="Remote and docker host" className="flex flex-col gap-1 rounded-lg border bg-background p-1.5">
      {state.remote ? (
        <DropdownMenu>
          <DropdownMenuTrigger render={<Button variant="ghost" className="h-auto w-full justify-between px-2 py-1.5 text-left" />}>
            <span className="min-w-0">
              <span className="flex items-center gap-1.5 text-[11px] text-muted-foreground">
                Remote
                {state.remote.error && <Dot tone="bad" label="remote error" />}
              </span>
              <span className="block truncate font-mono text-[13px]">
                {state.remote.name ?? "remote"} · {state.namespaces.current || state.remote.ns}
              </span>
            </span>
            <ChevronsUpDown className="size-3.5 text-muted-foreground" />
          </DropdownMenuTrigger>
          <DropdownMenuContent align="start" side="top" className="w-64">
            <DropdownMenuGroup>
              <DropdownMenuLabel className="font-mono text-[11px] break-all">{state.remote.url}</DropdownMenuLabel>
            </DropdownMenuGroup>
            {remoteActions.length > 0 && <DropdownMenuSeparator />}
            <ActionItems actions={state.namespaces.actions} subject="namespace" />
            {(state.remotes?.actions.length ?? 0) > 0 && state.namespaces.actions.length > 0 && <DropdownMenuSeparator />}
            <ActionItems actions={state.remotes?.actions ?? []} subject="remote" />
            {(state.controller_actions?.length ?? 0) > 0 && <DropdownMenuSeparator />}
            <ActionItems actions={state.controller_actions ?? []} subject="controller" />
          </DropdownMenuContent>
        </DropdownMenu>
      ) : (
        <div className="px-2 py-1.5">
          <div className="text-[11px] text-muted-foreground">Remote</div>
          <div className="text-[13px] text-muted-foreground">none</div>
        </div>
      )}
      {state.hosts.all.length > 0 && (
        <DropdownMenu>
          <DropdownMenuTrigger render={<Button variant="ghost" className="h-auto w-full justify-between px-2 py-1.5 text-left" />}>
            <span className="min-w-0">
              <span className="block text-[11px] text-muted-foreground">Docker host</span>
              <span className="block truncate font-mono text-[13px]">{host}</span>
            </span>
            <ChevronsUpDown className="size-3.5 text-muted-foreground" />
          </DropdownMenuTrigger>
          <DropdownMenuContent align="start" side="top" className="w-64">
            <ActionItems actions={state.hosts.actions} subject="docker host" />
          </DropdownMenuContent>
        </DropdownMenu>
      )}
    </section>
  )
}
