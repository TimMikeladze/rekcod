import type { ReactNode } from "react"
import { CopyCommand } from "@/components/copy-command"

/**
 * An empty list's first steps: what it would show, and the commands that fill it, each with a
 * copy button. Nothing runs from here.
 */
export function GetStarted({ title, children, commands }: { title: string; children: ReactNode; commands: string[][] }) {
  return (
    <div className="m-3 rounded-lg border border-dashed px-6 py-8">
      <div className="mx-auto max-w-[560px] space-y-3">
        <div>
          <div className="text-[15px] font-medium">{title}</div>
          <p className="cap mt-0.5">{children}</p>
        </div>
        <div className="space-y-1.5">
          {commands.map((c) => (
            <CopyCommand key={c.join(" ")} args={c} label={c.slice(0, 2).join(" ")} />
          ))}
        </div>
      </div>
    </div>
  )
}

/** Apps, Jobs and Fleet without a remote: how to add one. */
export function NoRemote({ what }: { what: string }) {
  return (
    <GetStarted
      title="No fleet remote"
      commands={[
        ["remote", "add", "team", "http://controller:7700", "--token", "ADMIN_TOKEN"],
        ["host", "add", "mini", "me@mac-mini", "--server", "--domain", "apps.example.com"],
      ]}
    >
      {what} live on a fleet controller. Point rekcod at one, or turn another Mac into a one-node server.
    </GetStarted>
  )
}
