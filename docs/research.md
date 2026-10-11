# Phase 6 research

Two questions from `docs/handoff.md`. Each item gives a decision, not a project.

## 1. Would our own file server beat Apple's virtio-fs on cold metadata?

**Why ask:** cold first-touch is the one place rekcod loses to OrbStack
(`phase2.md`: `walk` 4–6× slower cold, ~25–60 µs per host request).

**Prototype:** the daemon (with `REKCOD_RESEARCH=1`) parks a vsock connection
on `META_PORT` and answers `lstat`/`readdir` lines with the Mac's own syscalls.
`rekcod research bench-meta <dir>` makes the agent time the same cold `lstat`s
both ways: through that server, and through virtio-fs after dropping the guest's
caches for `<dir>` (subtree `rekcod_notify` + `drop_caches`). Code:
`crates/rekcod-agent/src/meta.rs`, `daemon.rs` `meta_server`.

This is a lower bound for a custom server: a real one also pays the guest-side
FUSE (or kernel-module) hop and must handle open/read/write, locking and
invalidation. If even the bare request/reply over vsock isn't clearly faster,
building one isn't worth it.

**Measured:** 5,125 cold lstats over a 20k-file tree: **63 µs** per request
through the vsock server vs **46 µs** through virtio-fs. A readdir over vsock: 199 µs.

**Decision: don't build a file server.** Apple's server isn't the bottleneck per
request; the vsock round trip from the guest alone costs as much. Cold first
touch is better fixed by doing the round trips early and in parallel
(phase 4 warming: 6× on a 20k-file tree).

## 2. Would a custom VMM (libkrun) give us memory release?

Desk research, no prototype.

| | Virtualization.framework (today) | libkrun / krunkit on Hypervisor.framework |
|---|---|---|
| Free page reporting | no; the traditional balloon inflates but macOS 27 keeps the footprint (`phase2.md`) | yes (virtio-balloon free page reporting; podman machine uses it via krunkit) |
| virtio-fs | Apple's server, ~25–60 µs/request, no DAX | libkrun's own server in Rust, DAX window possible |
| Rosetta for amd64 | yes (`VZLinuxRosettaDirectoryShare`) | no: Rosetta for Linux is only offered through VZ. amd64 would need QEMU user mode or FEX, both several times slower than Rosetta for typical images |
| GPU | no | venus/virtio-gpu (krunkit) |
| Maintenance | Apple ships it | we'd own a VMM's device models |

**Decision:** stay on VZ. Rosetta is a top switcher requirement (amd64-only
images are still common), and the memory problem has cheaper levers first:
page-cache reclaim under pressure. (`init_on_free=1` was measured and
rejected: it zeroes all guest RAM at boot, so the footprint starts at the full
VM size; `phase2b-4.md`.) Revisit only if Apple drops Rosetta for Linux or if
macOS's VZ balloon starts releasing memory.

