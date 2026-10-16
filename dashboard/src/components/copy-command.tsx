"use client"

import { useState } from "react"
import { Check, Copy } from "lucide-react"
import { Button } from "@/components/ui/button"

const q = (s: string) => (/^[A-Za-z0-9_\-./:=@+,]+$/.test(s) ? s : `'${s.replace(/'/g, `'\\''`)}'`)

/** `rekcod` plus `args`, shell-quoted. */
export function commandLine(args: string[]): string {
  return ["rekcod", ...args].map(q).join(" ")
}

/**
 * A CLI-only command (it takes arguments the menu bar can't ask for), shown with a copy button.
 * Nothing runs from here.
 */
export function CopyCommand({ args, label }: { args: string[]; label?: string }) {
  const [copied, setCopied] = useState(false)
  const line = commandLine(args)
  return (
    <div className="flex items-center gap-1 rounded-lg border bg-subtle py-1 pr-1 pl-2.5">
      <code className="min-w-0 flex-1 truncate font-mono text-xs" title={line}>
        {line}
      </code>
      <Button
        size="xs"
        variant="ghost"
        aria-label={label ? `Copy: ${label}` : "Copy command"}
        onClick={async () => {
          try {
            await navigator.clipboard.writeText(line)
            setCopied(true)
            setTimeout(() => setCopied(false), 1500)
          } catch {}
        }}
      >
        {copied ? <Check /> : <Copy />}
        {copied ? "Copied" : "Copy"}
      </Button>
    </div>
  )
}
