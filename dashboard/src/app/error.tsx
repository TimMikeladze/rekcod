"use client"

import { PageTitle } from "@/components/shell/page-title"
import { Button } from "@/components/ui/button"

// A page that threw while rendering. The shell around it keeps working.
export default function PageError({ error, retry }: { error: Error & { digest?: string }; retry: () => void }) {
  return (
    <>
      <PageTitle title="Something broke">This page threw while rendering. The rest of the dashboard still works.</PageTitle>
      <pre className="overflow-x-auto rounded-md border bg-muted p-3 font-mono text-xs whitespace-pre-wrap">
        {error.message || "no message"}
        {error.digest ? `\ndigest ${error.digest} (the server log has the stack)` : ""}
      </pre>
      <div>
        <Button variant="outline" onClick={() => retry()}>
          Try again
        </Button>
      </div>
    </>
  )
}
