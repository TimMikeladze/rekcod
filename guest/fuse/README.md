# Patched FUSE / virtio-fs (guest)

`rekcod-fuse.patch` (against Linux `fs/fuse` at the guest kernel's version) and
`rekcod.c` (new) are applied to the kernel tree in the `custom-kernel` stage of
`guest/Dockerfile`, so fuse and virtiofs are built into the kernel. With
`--build-arg KERNEL=alpine` they're built out of tree as `fuse.ko`, `virtiofs.ko` and
`cuse.ko` instead, replacing Alpine's stock modules in the initramfs. Everything is gated on the virtio-fs `rekcod` mount option.
Design and measurements: `docs/phase2.md`.

To change the patch:

```sh
V=6.12.112   # guest kernel version (ls /lib/modules in the guest)
curl -sL https://cdn.kernel.org/pub/linux/kernel/v6.x/linux-$V.tar.xz | tar -xJ linux-$V/fs/fuse
cp -R linux-$V/fs/fuse a && cp -R linux-$V/fs/fuse b
(cd b && patch -p1 < ../rekcod-fuse.patch)
# edit b/..., then:
diff -ruN a b | sed 's|^--- a/|--- a/|; s|^+++ b/|+++ b/|' > rekcod-fuse.patch
```

When Alpine moves the kernel, the guest build fetches the matching kernel.org
sources automatically. Re-check that the patch still applies.
