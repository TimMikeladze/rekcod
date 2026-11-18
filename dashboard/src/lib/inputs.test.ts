import { describe, expect, it } from "vitest"
import vectors from "../../fixtures/input-vectors.json"
import fixture from "../../fixtures/state.json"
import { fill, invalid, splitValues } from "./inputs"
import type { Action, Input, InputKind, State } from "./types"

const s = fixture as unknown as State
const engine = (kind: string) => s.engine_actions!.find((a) => a.kind === kind)! as Action & { argv: string[] }

describe("fill", () => {
  it("puts each value after its flag and leaves out empty optional slots", () => {
    const run = engine("ImageRun")
    expect(fill(run.argv, run.inputs!, { image: ["nginx:1.29"], publish: ["8080:80", "127.0.0.1:8443:443/tcp"] })).toEqual({
      argv: ["run", "-d", "-p", "8080:80", "-p", "127.0.0.1:8443:443/tcp", "nginx:1.29"],
    })
  })

  it("refuses missing, repeated and flag-like values", () => {
    const run = engine("ImageRun")
    expect(fill(run.argv, run.inputs!, {})).toEqual({ error: "Image is required" })
    expect(fill(run.argv, run.inputs!, { image: ["a"], name: ["x", "y"] })).toEqual({ error: "Container name takes one value" })
    expect(fill(run.argv, run.inputs!, { image: ["--privileged"] })).toMatchObject({ error: expect.stringContaining("can't start with -") })
  })

  it("replaces a slot inside a token", () => {
    const clone = s.volumes[0].actions.find((a) => a.kind === "VolumeClone")!
    const f = fill(clone.argv!, clone.inputs!, { target: ["copy"] })
    expect("argv" in f && f.argv).toContain("copy:/to")
    // Taken as is, every time the slot appears, as Rust's `str::replace` does.
    const env: Input = { key: "t", label: "T", kind: "env", optional: false, multiple: false, flag: null, placeholder: "" }
    expect(fill(["{t}:{t}"], [env], { t: ["A=$&-$'"] })).toEqual({ argv: ["A=$&-$':A=$&-$'"] })
  })
})

describe("invalid", () => {
  it("gives actions::invalid's answers (fixtures/input-vectors.json)", () => {
    for (const [k, v, valid] of vectors as [InputKind, string, boolean][]) expect(invalid(k, v) === null, `${k} ${JSON.stringify(v)}`).toBe(valid)
  })

  it("refuses a kind it doesn't know", () => {
    expect(invalid("bogus" as never, "x")).not.toBeNull()
  })

  it("matches actions::invalid", () => {
    for (const [k, v] of [
      ["image", "ghcr.io/me/app:1.2"],
      ["name", "pg_data-1.x"],
      ["publish", "5353:53/udp"],
      ["env", "A_B=x y=z"],
      ["mount", "/Users/me/src:/src"],
      ["mount", "data:/data:ro"],
    ] as const)
      expect(invalid(k, v), `${k} ${v}`).toBeNull()
    for (const [k, v] of [
      ["image", "a b"],
      ["name", "_x"],
      ["publish", "70000:80"],
      ["publish", "300.1.1.1:80:80"],
      ["env", "1A=x"],
      ["env", "A=x\ny"],
      ["mount", "data:rel"],
    ] as const)
      expect(invalid(k, v), `${k} ${v}`).not.toBeNull()
  })

  it("splits repeatable fields by line", () => {
    const publish = engine("ImageRun").inputs!.find((i) => i.key === "publish")!
    expect(splitValues(publish, " 80:80\n\n443:443 ")).toEqual(["80:80", "443:443"])
  })
})
