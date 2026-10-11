# Patched FUSE / virtio-fs (guest)

`rekcod-fuse.patch` (against Linux `fs/fuse` at the guest kernel's version) and
`rekcod.c` (new) build `fuse.ko`, `virtiofs.ko` and `cuse.ko` in the `fuse` stage
of `guest/Dockerfile`. They replace Alpine's stock modules and get rebuilt into the
initramfs. Everything is gated on the virtio-fs `rekcod` mount option.
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

When Alpine moves the kernel, `scripts/build.sh` fetches the matching kernel.org
sources automatically. Re-check that the patch still applies.
