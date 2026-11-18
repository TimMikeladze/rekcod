import "server-only"
import { once } from "./bus"

// The engine this dashboard server shows: `local` (this Mac's VM) or a host's name. One per
// server, like a docker context; the sidebar switches it (POST /api/engine).
const e = once<{ name: string }>("engine", () => ({ name: "local" }))

export const engine = () => e.name

export function setEngine(name: string) {
  e.name = name
}

/** `argv` for the engine: a host's runs there through `rekcod host exec`. */
export function onEngine(argv: string[], name = e.name): string[] {
  return name === "local" ? argv : ["host", "exec", name, "--", ...argv]
}
