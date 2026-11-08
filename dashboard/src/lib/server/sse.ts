import "server-only"

/** Bytes an event stream queues for a page that stops reading before it starts coalescing. */
export const HIGH_WATER = 256 * 1024
/** Distinct frames held for a page that's behind; one more and the page is dropped (it reconnects). */
export const MAX_PENDING = 64

type Send = (event: string, data: unknown) => void

/**
 * A server-sent events body for one page. `setup` subscribes `send` to whatever it forwards
 * and returns the unsubscribe, which runs when the page goes away or is dropped.
 *
 * While the page reads, frames go straight out. Once HIGH_WATER bytes wait unread, each
 * event keeps only its newest frame (each activity entry its own) until the page catches up:
 * `state` and `stats` are whole snapshots, so the newest is all a late page needs. More than
 * MAX_PENDING waiting frames errors the stream, which drops the page.
 */
export function eventStream(signal: AbortSignal, setup: (send: Send, ping: () => void) => () => void): ReadableStream<Uint8Array> {
  const enc = new TextEncoder()
  const pending = new Map<string, Uint8Array>()
  let ctl: ReadableStreamDefaultController<Uint8Array>
  let closed = false
  let teardown = () => {}
  const room = () => !closed && (ctl.desiredSize ?? 0) > 0
  const stop = () => {
    if (closed) return
    closed = true
    pending.clear()
    teardown()
  }
  const send: Send = (event, data) => {
    if (closed) return
    const frame = enc.encode(`event: ${event}\ndata: ${JSON.stringify(data)}\n\n`)
    if (!pending.size && room()) return ctl.enqueue(frame)
    const key = event === "activity" ? `activity ${(data as { id?: unknown })?.id}` : event
    pending.delete(key)
    pending.set(key, frame)
    if (pending.size > MAX_PENDING) {
      ctl.error(new Error("event stream reader fell behind"))
      stop()
    }
  }
  const ping = () => {
    if (!pending.size && room()) ctl.enqueue(enc.encode(": ping\n\n"))
  }
  return new ReadableStream<Uint8Array>(
    {
      start(c) {
        ctl = c
        teardown = setup(send, ping)
        signal.addEventListener("abort", () => {
          if (closed) return
          stop()
          try {
            c.close()
          } catch {}
        })
      },
      pull() {
        for (const [key, frame] of pending) {
          if (!room()) break
          pending.delete(key)
          ctl.enqueue(frame)
        }
      },
      cancel: stop,
    },
    new ByteLengthQueuingStrategy({ highWaterMark: HIGH_WATER }),
  )
}
