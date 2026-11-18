import { mkdtempSync, readFileSync, rmSync, statSync } from "node:fs"
import os from "node:os"
import path from "node:path"
import { describe, expect, it } from "vitest"
import { appleScript, terminalScript, writeScript } from "./terminal"

describe("Terminal", () => {
  it("runs a script file, not typed keystrokes", () => {
    const s = terminalScript("/opt/rekcod", ["host", "exec", "mini", "--", "logs", "it's", "=x"], "/Users/me/my app")
    expect(s).toBe(`#!/bin/sh\nrm -f -- "$0"; rmdir -- "\${0%/*}" 2>/dev/null\ncd '/Users/me/my app' || exit\nexec /opt/rekcod host exec mini -- logs 'it'\\''s' =x\n`)
    expect(appleScript('/tmp/x y/"a".sh')).toBe(`tell application "Terminal"\nactivate\ndo script "'/tmp/x y/\\"a\\".sh'"\nend tell`)
  })
  it("refuses control characters anywhere", () => {
    for (const bad of ["\u0015/usr/bin/touch x\n#", "\u0003", "\u007f", "\u009b"]) {
      expect(terminalScript("/opt/rekcod", ["logs", bad])).toBeNull()
      expect(terminalScript("/opt/rekcod", ["logs"], `/tmp/${bad}`)).toBeNull()
    }
    expect(appleScript("/tmp/\u0015.sh")).toBeNull()
  })
  it("writes the script 0700 in a private directory", () => {
    const base = mkdtempSync(path.join(os.tmpdir(), "rekcod-term-test-"))
    try {
      const file = writeScript("#!/bin/sh\nexit 0\n", base)
      expect(readFileSync(file, "utf8")).toBe("#!/bin/sh\nexit 0\n")
      expect(statSync(file).mode & 0o777).toBe(0o700)
      expect(statSync(path.dirname(file)).mode & 0o777).toBe(0o700)
    } finally {
      rmSync(base, { recursive: true, force: true })
    }
  })
})
