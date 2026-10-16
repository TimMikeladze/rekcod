"use client"

// Detail pages read their id in the browser: everything comes from the shell's live state,
// so there's nothing for the server to wait on.
import { Suspense } from "react"
import { useParams } from "next/navigation"
import { AppDetail } from "@/components/app-sheet"
import { ContainerDetail } from "@/components/container-sheet"
import { JobDetail } from "@/components/job-sheet"
import { NodeDetail } from "@/components/node-sheet"
import { SandboxDetail } from "@/components/sandbox-sheet"

const param = (p: Record<string, string | string[] | undefined>, k: string) => decodeURIComponent(String(p[k] ?? ""))

export function ContainerRoute() {
  const name = param(useParams(), "name")
  // The tabs read `?tab=`.
  return (
    <Suspense>
      <ContainerDetail name={name} />
    </Suspense>
  )
}
export function SandboxRoute() {
  return <SandboxDetail id={param(useParams(), "id")} />
}
export function AppRoute() {
  return <AppDetail name={param(useParams(), "name")} />
}
export function JobRoute() {
  return <JobDetail id={param(useParams(), "id")} />
}
export function NodeRoute() {
  return <NodeDetail id={param(useParams(), "id")} />
}
