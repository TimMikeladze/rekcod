import type { Action } from "./types"

/** Every action in the document, wherever it sits. */
export function allActions(v: unknown, out: Action[] = []): Action[] {
  if (Array.isArray(v)) v.forEach((x) => allActions(x, out))
  else if (v && typeof v === "object") {
    const o = v as Record<string, unknown>
    if (typeof o.kind === "string" && typeof o.label === "string" && ("argv" in o || "url" in o)) out.push(o as Action)
    else Object.values(o).forEach((x) => allActions(x, out))
  }
  return out
}

/** The action in `state` whose argv is exactly `argv`, or null. The API runs nothing else. */
export function findOffered(state: unknown, argv: unknown): Action | null {
  if (!Array.isArray(argv) || !argv.every((a) => typeof a === "string")) return null
  const key = JSON.stringify(argv)
  return allActions(state).find((a) => a.argv && JSON.stringify(a.argv) === key) ?? null
}
