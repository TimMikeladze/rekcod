# syntax=docker/dockerfile:1
# rekcod guest: Alpine linux-virt kernel + upstream static dockerd + rekcod-agent.
# Build with scripts/build.sh (passes the agent binary as the `agent` context).
ARG ALPINE=3.22
ARG DOCKER_VERSION=29.9.0
# Kernel source: empty = the Alpine release's linux-virt; or e.g.
# https://dl-cdn.alpinelinux.org/alpine/edge/main for a newer one.
ARG KERNEL_REPO=
# 0 = stock fuse/virtiofs modules (bisecting; boot.sh falls back to uncached sharing)
ARG FUSE_PATCH=1

FROM alpine:${ALPINE} AS rootfs
ARG DOCKER_VERSION
ARG KERNEL_REPO
RUN apk add --no-cache kmod e2fsprogs e2fsprogs-extra iptables ip6tables dnsmasq ca-certificates \
 && apk add --no-cache ${KERNEL_REPO:+--repository "$KERNEL_REPO"} linux-virt
ADD https://download.docker.com/linux/static/stable/aarch64/docker-${DOCKER_VERSION}.tgz /tmp/docker.tgz
RUN tar -xzf /tmp/docker.tgz -C /tmp \
 && cd /tmp/docker \
 && install -m 0755 dockerd containerd containerd-shim-runc-v2 runc docker-proxy docker-init /usr/local/bin/ \
 && rm -rf /tmp/docker /tmp/docker.tgz
COPY --from=agent rekcod-agent /usr/local/bin/rekcod-agent
COPY rootfs/ /
RUN chmod 0755 /etc/rekcod/*.sh \
 && mkdir -p /mnt/mac /mnt/rosetta /Users /Volumes /private /var/folders /var/lib/docker /etc/docker

# Patched fuse/virtiofs modules (guest/fuse, see docs/phase2.md), built against
# the exact linux-virt kernel installed above, from matching kernel.org sources.
FROM rootfs AS fuse
ARG KERNEL_REPO
RUN V=$(apk list -I linux-virt 2>/dev/null | sed -E 's/^linux-virt-([^ ]+) .*/\1/') \
 && apk add --no-cache build-base xz ${KERNEL_REPO:+--repository "$KERNEL_REPO"} "linux-virt-dev=$V"
RUN KREL=$(ls /lib/modules) && KV=${KREL%%-*} \
 && wget -qO /tmp/linux.tar.xz "https://cdn.kernel.org/pub/linux/kernel/v6.x/linux-$KV.tar.xz" \
 && mkdir /src && tar -xJf /tmp/linux.tar.xz -C /src --strip-components=2 "linux-$KV/fs/fuse" \
 && rm /tmp/linux.tar.xz
COPY fuse/ /patch/
ARG FUSE_PATCH
RUN set -eu; mkdir /out; [ "$FUSE_PATCH" = 1 ] || exit 0; KREL=$(ls /lib/modules); cd /src/fuse \
 && patch -p1 < /patch/rekcod-fuse.patch && cp /patch/rekcod.c . \
 && make -s -C "/usr/src/linux-headers-$KREL" M="$PWD" modules \
 && cp fuse.ko virtiofs.ko cuse.ko /out/

FROM rootfs AS rootfs-final
COPY --from=fuse /out/ /tmp/fuse/
RUN set -eu; KREL=$(ls /lib/modules); D="/lib/modules/$KREL/kernel/fs/fuse"; \
    for m in fuse virtiofs cuse; do [ -f "/tmp/fuse/$m.ko" ] || continue; gzip -9 -c "/tmp/fuse/$m.ko" > "$D/$m.ko.gz"; done; \
    rm -rf /tmp/fuse; depmod -a "$KREL"; \
    # the initramfs autoloads virtiofs for the share device; it must carry ours
    mkinitfs -o /boot/initramfs-virt "$KREL"

FROM alpine:${ALPINE} AS pack
RUN apk add --no-cache e2fsprogs
COPY --from=rootfs-final / /rootfs
# Alpine ships an EFI zboot vmlinuz; Virtualization.framework wants the raw arm64 Image.
RUN set -eu; mkdir /out; k=/rootfs/boot/vmlinuz-virt; \
    off=$(od -An -t u4 -j 8 -N 4 "$k" | tr -d ' '); len=$(od -An -t u4 -j 12 -N 4 "$k" | tr -d ' '); \
    tail -c +$((off + 1)) "$k" | head -c "$len" | gunzip > /out/Image; \
    [ "$(dd if=/out/Image bs=1 skip=56 count=4 2>/dev/null)" = "ARMd" ]; \
    cp /rootfs/boot/initramfs-virt /out/initramfs; rm -rf /rootfs/boot/*; \
    # content + 25% + 64 MB: the root only takes logs and /etc writes; docker
    # state is on the data disk. The release tarball compresses the rest away.
    mb=$(( $(du -sm /rootfs | cut -f1) * 5 / 4 + 64 )); \
    mke2fs -q -t ext4 -L rekcod-root -d /rootfs /out/root.img "${mb}M"; \
    cat /out/Image /out/initramfs /out/root.img | sha256sum | cut -c1-16 > /out/VERSION

FROM scratch AS out
COPY --from=pack /out/ /
