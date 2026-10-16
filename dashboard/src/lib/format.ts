const UNITS: Record<string, number> = {
  B: 1, kB: 1e3, KB: 1e3, MB: 1e6, GB: 1e9, TB: 1e12,
  KiB: 1024, MiB: 1024 ** 2, GiB: 1024 ** 3, TiB: 1024 ** 4,
}

/** "123.1MiB" → bytes, as `docker stats` prints them. */
export function parseBytes(s: string): number {
  const m = /^([\d.]+)\s*([A-Za-z]+)$/.exec(s.trim())
  return m ? Number(m[1]) * (UNITS[m[2]] ?? 1) : 0
}

export function fmtBytes(n: number | null | undefined): string {
  if (n == null || !Number.isFinite(n)) return "–"
  const u = ["B", "KiB", "MiB", "GiB", "TiB"]
  let i = 0
  while (n >= 1024 && i < u.length - 1) {
    n /= 1024
    i++
  }
  return `${n < 10 && i > 0 ? n.toFixed(1) : Math.round(n)} ${u[i]}`
}

export function ago(secs: number | null | undefined, now = Date.now() / 1000): string {
  if (!secs) return "never"
  const a = Math.max(0, Math.round(now - secs))
  if (a < 60) return `${a}s ago`
  if (a < 3600) return `${Math.floor(a / 60)}m ago`
  if (a < 172_800) return `${Math.floor(a / 3600)}h ago`
  return `${Math.floor(a / 86_400)}d ago`
}

/** 310 → "5m 10s", 86400 → "1d"; the two largest units. */
export function duration(secs: number): string {
  if (!secs) return "off"
  const parts: string[] = []
  for (const [n, u] of [[86_400, "d"], [3600, "h"], [60, "m"], [1, "s"]] as const) {
    if (secs >= n) {
      parts.push(`${Math.floor(secs / n)}${u}`)
      secs %= n
    }
  }
  return parts.slice(0, 2).join(" ")
}
