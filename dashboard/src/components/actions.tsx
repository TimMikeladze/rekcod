"use client";

import { createContext, useContext, useState, type ReactNode } from "react";
import { toast } from "sonner";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input as TextInput } from "@/components/ui/input";
import { fill, invalid, splitValues, type Values } from "@/lib/inputs";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { useRouter } from "next/navigation";
import { useDash } from "@/hooks/dash-context";
import { api } from "@/lib/api";
import { destructive, STREAMED } from "@/lib/kinds";
import type { Action } from "@/lib/types";
import { Loader2, MoreHorizontal } from "lucide-react";

type Item = { a: Action; subject: string };
type Ctx = {
  trigger: (a: Action, subject: string) => void;
  /** Several rows' own actions (bulk bar): one confirm for all, then each runs in turn. */
  triggerMany: (items: Item[], what: string, onStart?: () => void) => void;
  /** argv keys (JSON) of the actions running now. */
  pending: ReadonlySet<string>;
};
const ActionCtx = createContext<Ctx>({ trigger: () => {}, triggerMany: () => {}, pending: new Set() });

type RunResult = { ok: boolean; out: string; activity?: number };

/** POST /api/run; the server checks the argv against what `rekcod state` offers now (and fills in `values`). */
async function post(a: Action, subject: string, values?: Values): Promise<RunResult> {
  try {
    const r = await api("/api/run", {
      method: "POST",
      headers: { "content-type": "application/json", "x-rekcod": "1" },
      body: JSON.stringify({ argv: a.argv, subject, values }),
    });
    const body = await r.json().catch(() => ({ error: r.statusText }));
    if (!r.ok || body.code !== 0)
      return { ok: false, out: (body.error ?? body.stderr ?? "").trim().slice(-500) || `exit ${body.code}`, activity: body.activity };
    return { ok: true, out: (body.stdout || body.stderr || "done").trim().slice(-300), activity: body.activity };
  } catch (e) {
    return { ok: false, out: String(e) };
  }
}

const runHref = (id?: number) => (id ? `/activity?run=${id}` : "/activity");

/**
 * Runs actions for everything below it: confirm dialogs, the stream page, toasts. The server
 * records each run in Activity; each toast links to its run there.
 */
export function ActionProvider({
  children,
  onDone,
}: {
  children: ReactNode;
  onDone: () => void;
}) {
  const [confirm, setConfirm] = useState<{ items: Item[]; what: string; onStart?: () => void } | null>(null);
  const [ask, setAsk] = useState<Item | null>(null);
  const [pending, setPending] = useState<ReadonlySet<string>>(new Set());
  const router = useRouter();
  const mark = (key: string, on: boolean) =>
    setPending((p) => {
      const n = new Set(p);
      if (on) n.add(key);
      else n.delete(key);
      return n;
    });

  async function exec(a: Action, subject: string, values?: Values) {
    if (a.url) {
      window.open(a.url, "_blank", "noopener");
      return;
    }
    if (STREAMED.has(a.kind)) {
      router.push(streamHref(a, subject));
      return;
    }
    const key = JSON.stringify(a.argv);
    mark(key, true);
    const shown = values && a.inputs ? fill(a.argv!, a.inputs, values) : null;
    const id = toast.loading(`${a.label} · ${subject}`, {
      description: `rekcod ${(shown && "argv" in shown ? shown.argv : a.argv!).join(" ")}`,
    });
    const r = await post(a, subject, values);
    const view = { label: r.ok ? "View" : "Details", onClick: () => router.push(runHref(r.activity)) };
    if (r.ok) toast.success(`${a.label} · ${subject}`, { id, description: r.out, action: view });
    else toast.error(`${a.label} failed`, { id, description: r.out, duration: 10_000, action: view });
    mark(key, false);
    onDone();
  }

  async function execMany(items: Item[], what: string) {
    const runs = items.filter((i) => i.a.argv && !i.a.url && !STREAMED.has(i.a.kind) && !i.a.inputs?.length);
    if (!runs.length) return;
    const label = runs[0].a.label;
    const id = toast.loading(`${label} · ${what}`, { description: `0 of ${runs.length}` });
    runs.forEach((i) => mark(JSON.stringify(i.a.argv), true));
    let failed = 0;
    let firstBad: number | undefined;
    for (const [n, i] of runs.entries()) {
      const r = await post(i.a, i.subject);
      mark(JSON.stringify(i.a.argv), false);
      if (!r.ok) {
        failed++;
        firstBad ??= r.activity;
      }
      toast.loading(`${label} · ${what}`, { id, description: `${n + 1} of ${runs.length}` });
    }
    if (failed)
      toast.error(`${label}: ${failed} of ${runs.length} failed`, {
        id,
        description: "Each run is in Activity.",
        duration: 10_000,
        action: { label: "Details", onClick: () => router.push(runHref(firstBad)) },
      });
    else toast.success(`${label} · ${what}`, { id, description: `${runs.length} done`, action: { label: "View", onClick: () => router.push("/activity") } });
    onDone();
  }

  const trigger = (a: Action, subject: string) =>
    a.inputs?.length ? setAsk({ a, subject }) : a.confirm ? setConfirm({ items: [{ a, subject }], what: subject }) : exec(a, subject);
  /** `onStart` runs once the commands start (after the confirm, not on cancel). */
  const triggerMany = (items: Item[], what: string, onStart?: () => void) => {
    if (items.some((i) => i.a.confirm)) return setConfirm({ items, what, onStart });
    onStart?.();
    if (items.length === 1) exec(items[0].a, items[0].subject);
    else execMany(items, what);
  };

  const one = confirm?.items.length === 1 ? confirm.items[0] : null;
  const first = confirm?.items[0];
  return (
    <ActionCtx.Provider value={{ trigger, triggerMany, pending }}>
      {children}
      {ask && <InputDialog key={JSON.stringify(ask.a.argv)} item={ask} onClose={() => setAsk(null)} onRun={(values) => exec(ask.a, ask.subject, values)} />}
      <AlertDialog
        open={!!confirm}
        onOpenChange={(o) => !o && setConfirm(null)}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>{one ? one.a.label : `${first?.a.label} ${confirm?.what}`}</AlertDialogTitle>
            <AlertDialogDescription>
              {one ? one.a.confirm : `Runs ${confirm?.items.length} commands, one after another:`}
            </AlertDialogDescription>
          </AlertDialogHeader>
          {one ? (
            <code className="rounded bg-muted px-2 py-1 font-mono text-xs break-all">
              rekcod {one.a.argv?.join(" ")}
            </code>
          ) : (
            <ul className="max-h-60 space-y-1 overflow-y-auto">
              {confirm?.items.map((i) => (
                <li key={JSON.stringify(i.a.argv)}>
                  <code className="block rounded bg-muted px-2 py-1 font-mono text-xs break-all">rekcod {i.a.argv?.join(" ")}</code>
                  {i.a.confirm && <span className="text-xs text-muted-foreground">{i.a.confirm}</span>}
                </li>
              ))}
            </ul>
          )}
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction
              variant={
                first && destructive(first.a)
                  ? "destructive"
                  : "default"
              }
              onClick={() => {
                const c = confirm!;
                setConfirm(null);
                c.onStart?.();
                if (c.items.length === 1) exec(c.items[0].a, c.items[0].subject);
                else execMany(c.items, c.what);
              }}
            >
              {one ? one.a.label : `${first?.a.label} ${confirm?.items.length}`}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </ActionCtx.Provider>
  );
}

/**
 * The form for an action's inputs: a field per input (a text area, one value per line, for
 * repeatable ones), checked with the rules /api/run applies, and the command it will run.
 */
function InputDialog({ item, onClose, onRun }: { item: Item; onClose: () => void; onRun: (values: Values) => void }) {
  const { a, subject } = item;
  const inputs = a.inputs ?? [];
  const [text, setText] = useState<Record<string, string>>({});
  const [tried, setTried] = useState(false);
  const values: Values = Object.fromEntries(inputs.map((i) => [i.key, splitValues(i, text[i.key] ?? "")]));
  const filled = fill(a.argv!, inputs, values);
  const fieldError = (key: string) => {
    const i = inputs.find((x) => x.key === key)!;
    const vals = values[key];
    if (!vals.length) return tried && !i.optional ? "Required" : null;
    for (const v of vals) {
      const why = invalid(i.kind, v);
      if (why) return `${v}: ${why}`;
    }
    return null;
  };
  const submit = () => {
    setTried(true);
    if ("error" in filled) return;
    onClose();
    onRun(values);
  };
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{a.label}</DialogTitle>
          <DialogDescription>{subject}</DialogDescription>
        </DialogHeader>
        <form
          className="flex flex-col gap-3"
          onSubmit={(e) => {
            e.preventDefault();
            submit();
          }}
        >
          {inputs.map((i, n) => {
            const err = fieldError(i.key);
            const id = `input-${i.key}`;
            const common = {
              id,
              name: i.key,
              value: text[i.key] ?? "",
              placeholder: i.placeholder,
              autoFocus: n === 0,
              "aria-invalid": !!err || undefined,
              "aria-describedby": err ? `${id}-err` : undefined,
              spellCheck: false,
              autoComplete: "off",
            };
            return (
              <div key={i.key} className="flex flex-col gap-1.5">
                <label htmlFor={id} className="text-sm font-medium">
                  {i.label}
                  {i.optional && <span className="font-normal text-muted-foreground"> (optional)</span>}
                  {i.multiple && <span className="font-normal text-muted-foreground">, one per line</span>}
                </label>
                {i.multiple ? (
                  <textarea
                    {...common}
                    rows={2}
                    onChange={(e) => setText((t) => ({ ...t, [i.key]: e.target.value }))}
                    className="min-h-16 w-full rounded-lg border border-input bg-transparent px-2.5 py-1.5 font-mono text-sm outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50 aria-invalid:border-destructive"
                  />
                ) : (
                  <TextInput {...common} className="font-mono" onChange={(e) => setText((t) => ({ ...t, [i.key]: e.target.value }))} />
                )}
                {err && (
                  <span id={`${id}-err`} className="text-xs text-alert-text">
                    {err}
                  </span>
                )}
              </div>
            );
          })}
          <code className="rounded bg-muted px-2 py-1 font-mono text-xs break-all" aria-label="Command">
            rekcod {("argv" in filled ? filled.argv : a.argv!).join(" ")}
          </code>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={onClose}>
              Cancel
            </Button>
            <Button type="submit">{a.label}</Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

/** The page that streams a log-like action's output (`/stream`). */
export function streamHref(a: Action, subject: string) {
  return `/stream?argv=${encodeURIComponent(JSON.stringify(a.argv))}&title=${encodeURIComponent(`${a.label} · ${subject}`)}`;
}

export function useAction() {
  return useContext(ActionCtx);
}

/**
 * Row actions: the first `inline` as buttons, the rest in a menu. Destructive ones always go
 * in the menu, after a separator. Below `sm` (phones) every row action is in the menu;
 * `toolbar` (the top of a detail page) keeps outlined buttons at every width.
 */
export function Actions({
  actions,
  subject,
  inline = 2,
  toolbar = false,
}: {
  actions: Action[];
  subject: string;
  inline?: number;
  toolbar?: boolean;
}) {
  const { trigger, pending } = useAction();
  const busy = actions.some((a) => a.argv && pending.has(JSON.stringify(a.argv)));
  const safe = actions.filter((a) => !destructive(a));
  const shown = safe.slice(0, inline);
  const rest = safe.slice(inline);
  const danger = actions.filter(destructive);
  if (!actions.length) return null;
  return (
    <div
      className={toolbar ? "flex flex-wrap items-center gap-2" : "flex items-center justify-end gap-1"}
      onClick={(e) => e.stopPropagation()}
      data-busy={busy || undefined}
      aria-busy={busy || undefined}
    >
      {busy && <Loader2 className="size-3.5 animate-spin text-muted-foreground" aria-label="Running" />}
      {shown.map((a) => (
        <Button
          key={a.kind + a.label}
          size={toolbar ? "sm" : "xs"}
          variant={toolbar ? "outline" : "ghost"}
          disabled={busy}
          onClick={() => trigger(a, subject)}
          title={a.argv ? `rekcod ${a.argv.join(" ")}` : a.url}
          aria-label={`${short(a)} ${subject}`}
          className={toolbar ? "" : "hidden sm:inline-flex"}
        >
          {short(a)}
        </Button>
      ))}
      <DropdownMenu>
        <DropdownMenuTrigger
          render={
            <Button
              size={toolbar ? "icon-sm" : "icon-xs"}
              variant={toolbar ? "outline" : "ghost"}
              aria-label={`More actions for ${subject}`}
              data-row-menu
              disabled={busy}
              className={rest.length + danger.length ? "" : toolbar ? "hidden" : "sm:hidden"}
            />
          }
        >
          <MoreHorizontal />
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end" className="min-w-48">
          {shown.map((a) => (
            <DropdownMenuItem
              key={a.kind + a.label}
              className={toolbar ? "hidden" : "sm:hidden"}
              onClick={() => trigger(a, subject)}
            >
              {a.label}
            </DropdownMenuItem>
          ))}
          {rest.map((a) => (
            <DropdownMenuItem
              key={a.kind + a.label}
              onClick={() => trigger(a, subject)}
            >
              {a.label}
            </DropdownMenuItem>
          ))}
          {rest.length > 0 && danger.length > 0 && <DropdownMenuSeparator />}
          {danger.map((a) => (
            <DropdownMenuItem
              key={a.kind + a.label}
              variant="destructive"
              onClick={() => trigger(a, subject)}
            >
              {a.label}
            </DropdownMenuItem>
          ))}
        </DropdownMenuContent>
      </DropdownMenu>
    </div>
  );
}

/** Labels like "Open http://localhost:3000" are too long for a row button. */
export function short(a: Action): string {
  if (a.kind === "OpenPort" || a.kind === "AppOpen") return "Open";
  if (a.kind === "AppRecycle") return "Restart";
  if (a.kind === "AppWake") return "Wake";
  if (a.kind === "AppRollback") return "Roll back";
  if (a.kind === "EdgeBan" && a.argv && a.argv.length > 2) return "Ban";
  if (a.kind === "EdgeUnban") return "Unban";
  if (a.kind === "RemoteUse") return "Use";
  if (a.kind === "BackupNow") return "Back up now";
  if (a.kind === "VolumeExport") return "Export";
  return a.label;
}

/** `engine_actions` of these kinds as buttons (Pull an image, Create a volume, ...). */
export function EngineBar({ kinds, note }: { kinds: string[]; note?: ReactNode }) {
  const { state } = useDash();
  const actions = (state?.engine_actions ?? []).filter((a) => kinds.includes(a.kind));
  if (!actions.length) return null;
  return (
    <div className="flex flex-wrap items-center gap-2 px-2 pt-2 text-xs text-muted-foreground">
      {note && <span>{note}</span>}
      <div className="ml-auto flex flex-wrap gap-1">
        {actions.map((a) => (
          <ActionButton key={a.kind} action={a} subject={state?.engine ?? "this Mac"} />
        ))}
      </div>
    </div>
  );
}

/** One action as a labelled button, for toolbars (prunes). Destructive ones are red and still confirm. */
export function ActionButton({ action, subject }: { action: Action; subject: string }) {
  const { trigger, pending } = useAction()
  const danger = destructive(action)
  return (
    <Button
      size="sm"
      variant="outline"
      disabled={!!action.argv && pending.has(JSON.stringify(action.argv))}
      onClick={() => trigger(action, subject)}
      title={action.argv ? `rekcod ${action.argv.join(" ")}` : action.url}
      className={danger ? "border-alert-border text-alert-text hover:bg-alert-bg" : ""}
    >
      {action.label}
    </Button>
  )
}
