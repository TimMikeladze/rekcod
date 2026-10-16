import { Suspense } from "react"
import { StreamPage } from "@/components/stream-page"

// Rendered from live client state; see the root layout.
export const instant = false

export default function Page() {
  return (
    <Suspense>
      <StreamPage />
    </Suspense>
  )
}
