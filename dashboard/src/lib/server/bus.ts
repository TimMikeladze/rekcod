import "server-only"

// SSE subscribers. Survives dev hot reloads on globalThis, like the other singletons.
type Send = (event: string, data: unknown) => void

export function once<T>(key: string, make: () => T): T {
  const g = globalThis as unknown as Record<string, T>
  return (g[`__rekcod_${key}`] ??= make())
}

const subs = once("subs", () => new Set<Send>())

export function subscribe(send: Send): () => void {
  subs.add(send)
  return () => subs.delete(send)
}

export function broadcast(event: string, data: unknown) {
  for (const s of subs) {
    try {
      s(event, data)
    } catch {
      subs.delete(s)
    }
  }
}

export const subscribers = () => subs.size
