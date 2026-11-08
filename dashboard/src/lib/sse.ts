/**
 * Reads a `text/event-stream` body, calling `on(event, data)` for each message, until the
 * stream ends. EventSource can't send an Authorization header, hence this. Handles what
 * /api/events writes: `event:` and `data:` fields, `:` comments, LF or CRLF line ends; `id:`
 * and `retry:` are ignored.
 */
export async function readEvents(body: ReadableStream<Uint8Array>, on: (event: string, data: string) => void): Promise<void> {
  const reader = body.getReader()
  const dec = new TextDecoder()
  let buf = ""
  let event = ""
  let data: string[] = []
  const line = (l: string) => {
    if (l.endsWith("\r")) l = l.slice(0, -1)
    if (l === "") {
      if (data.length) on(event || "message", data.join("\n"))
      event = ""
      data = []
      return
    }
    if (l.startsWith(":")) return
    const i = l.indexOf(":")
    const field = i < 0 ? l : l.slice(0, i)
    const value = i < 0 ? "" : l.slice(l[i + 1] === " " ? i + 2 : i + 1)
    if (field === "event") event = value
    else if (field === "data") data.push(value)
  }
  try {
    for (;;) {
      const { done, value } = await reader.read()
      if (done) break
      buf += dec.decode(value, { stream: true })
      let nl: number
      while ((nl = buf.indexOf("\n")) >= 0) {
        line(buf.slice(0, nl))
        buf = buf.slice(nl + 1)
      }
    }
  } finally {
    reader.releaseLock()
  }
}
