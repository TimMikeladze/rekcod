import "server-only"
import { mkdtempSync, writeFileSync } from "node:fs"
import os from "node:os"
import path from "node:path"

// Terminal.app is handed the path of a script this server wrote, never the command itself:
// `do script` types its text into the window's line editor, where a ^U or ^C inside quotes
// discards the line so far and the rest runs as a new command. The menu bar does the same
// (menubar.rs `in_terminal`).

/** C0, DEL or C1: bytes a terminal acts on. */
export const hasControl = (s: string) => /[\u0000-\u001f\u007f-\u009f]/.test(s)

export const shellQuote = (s: string) => (/^[A-Za-z0-9_\-./:=@+,]+$/.test(s) ? s : `'${s.replace(/'/g, `'\\''`)}'`)

/**
 * The script for `bin argv` in `dir`, or null when any of it holds a control character. `/bin/sh`
 * rather than zsh, which expands a leading `=`; it deletes itself (and its directory) first.
 */
export function terminalScript(bin: string, argv: string[], dir?: string): string | null {
  if ([bin, ...argv, dir ?? ""].some(hasControl)) return null
  const cd = dir ? `cd ${shellQuote(dir)} || exit\n` : ""
  return `#!/bin/sh\nrm -f -- "$0"; rmdir -- "\${0%/*}" 2>/dev/null\n${cd}exec ${[bin, ...argv].map(shellQuote).join(" ")}\n`
}

/** The AppleScript that opens a Terminal window running the script at `file`, or null. */
export function appleScript(file: string): string | null {
  const line = shellQuote(file)
  if (hasControl(line)) return null
  return `tell application "Terminal"\nactivate\ndo script "${line.replace(/\\/g, "\\\\").replace(/"/g, '\\"')}"\nend tell`
}

/** Writes `script` 0700 in a new private directory under the user's tmpdir; its path. */
export function writeScript(script: string, base = os.tmpdir()): string {
  const dir = mkdtempSync(path.join(base, "rekcod-terminal-"))
  const file = path.join(dir, "run.sh")
  writeFileSync(file, script, { mode: 0o700, flag: "wx" })
  return file
}
