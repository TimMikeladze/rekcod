import { Activity, AppWindow, Boxes, Container, Cpu, Database, HardDrive, Layers, LayoutGrid, Network, Plug, Server, Timer, type LucideIcon } from "lucide-react"
import type { Tab } from "@/lib/issues"
import type { State } from "@/lib/types"

/** `go`: the letter after `g` that opens the page (`g c` is Containers). */
export type NavItem = { href: string; label: string; icon: LucideIcon; tab?: Tab; go: string }

/** The list pages, in the order 1-9 jump to them. */
export const SECTIONS: (NavItem & { tab: Tab; group: "This Mac" | "Fleet" })[] = [
  { tab: "containers", go: "c", href: "/containers", label: "Containers", icon: Container, group: "This Mac" },
  { tab: "images", go: "i", href: "/images", label: "Images", icon: Layers, group: "This Mac" },
  { tab: "volumes", go: "v", href: "/volumes", label: "Volumes", icon: Database, group: "This Mac" },
  { tab: "apps", go: "a", href: "/apps", label: "Apps", icon: AppWindow, group: "Fleet" },
  { tab: "jobs", go: "j", href: "/jobs", label: "Jobs", icon: Timer, group: "Fleet" },
  { tab: "sandboxes", go: "s", href: "/sandboxes", label: "Sandboxes", icon: Boxes, group: "This Mac" },
  { tab: "projects", go: "p", href: "/projects", label: "Projects", icon: Network, group: "This Mac" },
  { tab: "ports", go: "o", href: "/ports", label: "Ports", icon: Plug, group: "This Mac" },
  { tab: "fleet", go: "f", href: "/fleet", label: "Fleet", icon: Server, group: "Fleet" },
]

export const OVERVIEW: NavItem = { href: "/", label: "Overview", icon: LayoutGrid, go: "h" }

export const SYSTEM: NavItem[] = [
  { href: "/disk", label: "Disk", icon: HardDrive, go: "d" },
  { href: "/vm", label: "VM", icon: Cpu, go: "m" },
  { href: "/activity", label: "Activity", icon: Activity, go: "y" },
]

/** Every page in the sidebar, for `g` chords and the palette. */
export const PAGES: NavItem[] = [OVERVIEW, ...SECTIONS, ...SYSTEM]

/** Detail pages whose list lives under another name (`/nodes/x` is in Fleet). */
const ALIAS: Record<string, string> = { nodes: "fleet" }

/** The nav item a path belongs to: `/containers/web` is in Containers. */
export function sectionOf(path: string): NavItem | null {
  if (path === "/") return OVERVIEW
  const first = path.split("/")[1] ?? ""
  const key = ALIAS[first] ?? first
  return [...SECTIONS, ...SYSTEM].find((s) => s.href === `/${key}`) ?? null
}

/** The count beside each list in the sidebar; empty hides it. */
export function counts(s: State): Record<Tab, string> {
  const running = s.containers.filter((c) => c.state === "running").length
  return {
    containers: s.containers.length ? `${running}/${s.containers.length}` : "",
    images: String((s.images ?? []).length || ""),
    volumes: String((s.volumes ?? []).length || ""),
    apps: String(s.apps.length || ""),
    jobs: String(s.jobs.filter((j) => j.state === "running").length || s.jobs.length || ""),
    sandboxes: String(s.sandboxes.length || ""),
    projects: String(s.projects.length || ""),
    ports: String(s.ports.length || ""),
    fleet: s.nodes?.length ? String(s.nodes.length) : "",
  }
}
