// The dashboard's copy of `actions::invalid` and `actions::fill` (crates/rekcod/src/actions.rs).
// /api/run fills an offered action's `{key}` slots with values checked here; the form checks
// the same rules as you type. Keep the two in step.
import type { Input, InputKind } from "./types"

const NAME = /^[A-Za-z0-9][A-Za-z0-9_.-]{0,127}$/
const IMAGE = /^[A-Za-z0-9][A-Za-z0-9._/:@-]*$/
const ENV_KEY = /^[A-Za-z_][A-Za-z0-9_]*$/
const IPV4 = /^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/

const port = (s: string) => /^\d{1,5}$/.test(s) && Number(s) > 0 && Number(s) <= 65535
const ipv4 = (s: string) => IPV4.test(s) && s.split(".").every((p) => Number(p) <= 255)

const WHY: Record<InputKind, string> = {
  image: "isn't an image reference",
  name: "names start with a letter or digit and use letters, digits, _ . -",
  publish: "use HOST:CONTAINER, CONTAINER or IP:HOST:CONTAINER, optionally /udp",
  env: "use KEY=value",
  mount: "use VOLUME:/path or /mac/path:/path, optionally :ro",
}

/** Why `v` isn't a valid `kind` value, or null when it is. */
export function invalid(kind: InputKind, v: string): string | null {
  const max = kind === "env" ? 4096 : 255
  if (!v.length || v.length > max) return `must be 1 to ${max} characters`
  if (v.startsWith("-") || /[\u0000-\u001f\u007f]/.test(v)) return "can't start with - or contain control characters"
  let ok: boolean
  switch (kind) {
    case "image":
      ok = IMAGE.test(v)
      break
    case "name":
      ok = NAME.test(v)
      break
    case "publish": {
      const spec = v.replace(/\/(udp|tcp)$/, "")
      const p = spec.split(":")
      ok = p.length === 1 ? port(p[0]) : p.length === 2 ? port(p[0]) && port(p[1]) : p.length === 3 ? ipv4(p[0]) && port(p[1]) && port(p[2]) : false
      break
    }
    case "env": {
      const i = v.indexOf("=")
      ok = i > 0 && ENV_KEY.test(v.slice(0, i))
      break
    }
    case "mount": {
      const spec = v.replace(/:(ro|rw)$/, "")
      const i = spec.indexOf(":")
      const src = spec.slice(0, i)
      const dst = spec.slice(i + 1)
      ok = i > 0 && (NAME.test(src) || src.startsWith("/")) && dst.startsWith("/") && !src.includes(",") && !dst.includes(":") && !dst.includes(",")
      break
    }
  }
  return ok ? null : WHY[kind]
}

export type Values = Record<string, string[]>

/** `argv` with each input's `{key}` replaced by its checked values, or the first problem. */
export function fill(argv: string[], inputs: Input[], values: Values): { argv: string[] } | { error: string } {
  const checked = new Map<string, string[]>()
  for (const i of inputs) {
    const vals = (values[i.key] ?? []).map((v) => String(v).trim()).filter(Boolean)
    if (!vals.length && !i.optional) return { error: `${i.label} is required` }
    if (vals.length > 1 && !i.multiple) return { error: `${i.label} takes one value` }
    for (const v of vals) {
      const why = invalid(i.kind, v)
      if (why) return { error: `${i.label} ${JSON.stringify(v)}: ${why}` }
    }
    checked.set(i.key, vals)
  }
  const slot = (i: Input) => `{${i.key}}`
  const out: string[] = []
  for (const t of argv) {
    const whole = inputs.find((i) => t === slot(i))
    if (whole) {
      for (const v of checked.get(whole.key)!) {
        if (whole.flag) out.push(whole.flag)
        out.push(v)
      }
      continue
    }
    const inner = inputs.find((i) => t.includes(slot(i)))
    if (inner) {
      const v = checked.get(inner.key)![0]
      if (v == null) return { error: `${inner.label} is required` }
      out.push(t.replace(slot(inner), v))
      continue
    }
    out.push(t)
  }
  return { argv: out }
}

/** A field's text as values: one per line for repeatable inputs. */
export function splitValues(i: Input, text: string): string[] {
  const t = text.trim()
  if (!t) return []
  return i.multiple ? t.split("\n").map((l) => l.trim()).filter(Boolean) : [t]
}
