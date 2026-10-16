"use client"

// The root layout itself threw, so there's no shell or stylesheet; this brings its own.
export default function GlobalError({ error, retry }: { error: Error & { digest?: string }; retry: () => void }) {
  return (
    <html lang="en">
      <body style={{ font: "15px/1.5 system-ui, sans-serif", maxWidth: "40rem", margin: "15vh auto", padding: "0 16px", colorScheme: "light dark" }}>
        <title>Error · rekcod</title>
        <h1>The dashboard failed to load</h1>
        <pre style={{ whiteSpace: "pre-wrap", font: "13px ui-monospace, monospace", padding: 12, borderRadius: 6, background: "#8882" }}>
          {error.message || "no message"}
          {error.digest ? `\ndigest ${error.digest} (the server log has the stack)` : ""}
        </pre>
        <button type="button" onClick={() => retry()}>
          Try again
        </button>
      </body>
    </html>
  )
}
