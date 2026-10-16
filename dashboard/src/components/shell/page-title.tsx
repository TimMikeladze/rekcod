import type { ReactNode } from "react"

/** A page's title and one-line intro, with its actions on the right (dux's PageTitle). */
export function PageTitle({ title, children, actions }: { title: ReactNode; children?: ReactNode; actions?: ReactNode }) {
  return (
    <div className="flex flex-wrap items-end justify-between gap-3">
      <div className="min-w-0">
        <h1 className="flex min-w-0 items-center gap-2 text-[28px] font-semibold tracking-[-0.02em] break-all max-md:text-2xl">{title}</h1>
        {children && <div className="cap mt-1 max-w-[720px] text-sm">{children}</div>}
      </div>
      {actions && <div className="flex flex-wrap items-center gap-2 max-sm:w-full">{actions}</div>}
    </div>
  )
}

/** The top of a detail page: title, a monospace line under it, the item's actions. The top bar links back to the list. */
export function DetailHead({ title, sub, children }: { title: ReactNode; sub?: ReactNode; children?: ReactNode }) {
  return (
    <header className="space-y-1.5">
      <h1 className="flex min-w-0 items-center gap-2 text-[28px] font-semibold tracking-[-0.02em] break-all max-md:text-2xl">{title}</h1>
      {sub && <div className="font-mono text-xs break-all text-muted-foreground">{sub}</div>}
      {children && <div className="flex flex-wrap items-center gap-2 pt-2">{children}</div>}
    </header>
  )
}
