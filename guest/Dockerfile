# syntax=docker/dockerfile:1
# rekcod guest: kernel + Alpine userland + upstream static dockerd + rekcod-agent.
# Build with scripts/build.sh (passes the agent binary as the `agent` context).
ARG ALPINE=3.22
ARG DOCKER_VERSION=29.9.0
# custom = our kernel (docs/custom-kernel.md): kernel.org sources for the version
# Alpine ships, Alpine's config + guest/kernel/rekcod.config, fuse patch in-tree,
# no initramfs. alpine = Alpine's stock linux-virt with out-of-tree fuse modules.
ARG KERNEL=custom
# Kernel package source: empty = the Alpine release's linux-virt; or e.g.
# https://dl-cdn.alpinelinux.org/alpine/edge/main for a newer one.
ARG KERNEL_REPO=
# 0 = stock fuse/virtiofs (bisecting; boot.sh falls back to uncached sharing)
ARG FUSE_PATCH=1

FROM alpine:${ALPINE} AS rootfs
ARG DOCKER_VERSION
RUN apk add --no-cache kmod e2fsprogs e2fsprogs-extra iptables ip6tables dnsmasq ca-certificates
ADD https://download.docker.com/linux/static/stable/aarch64/docker-${DOCKER_VERSION}.tgz /tmp/docker.tgz
RUN tar -xzf /tmp/docker.tgz -C /tmp \
 && cd /tmp/docker \
 && install -m 0755 dockerd containerd containerd-shim-runc-v2 runc docker-proxy docker-init /usr/local/bin/ \
 && rm -rf /tmp/docker /tmp/docker.tgz
COPY --from=agent rekcod-agent /usr/local/bin/rekcod-agent
COPY rootfs/ /
RUN chmod 0755 /etc/rekcod/*.sh \
 && mkdir -p /mnt/mac /mnt/rosetta /Users /Volumes /private /var/folders /var/lib/docker /etc/docker

# Every kernel stage leaves /kout/Image (raw arm64), /kout/lib/modules and,
# only if the kernel needs one, /kout/initramfs.

# --- custom kernel -----------------------------------------------------------
FROM alpine:${ALPINE} AS custom-kernel
ARG KERNEL_REPO
ARG FUSE_PATCH
RUN apk add --no-cache build-base bc bison flex perl openssl openssl-dev elfutils-dev linux-headers \
      gmp-dev mpfr-dev mpc1-dev xz kmod diffutils findutils bash \
 && apk add --no-cache ${KERNEL_REPO:+--repository "$KERNEL_REPO"} linux-virt
RUN KREL=$(ls /lib/modules) && KV=${KREL%%-*} \
 && wget -qO /tmp/linux.tar.xz "https://cdn.kernel.org/pub/linux/kernel/v6.x/linux-$KV.tar.xz" \
 && mkdir /src && tar -xJf /tmp/linux.tar.xz -C /src --strip-components=1 \
 && rm /tmp/linux.tar.xz && cp /boot/config-*-virt /src/alpine.config
COPY fuse/ /patch/
COPY kernel/rekcod.config /src/rekcod.config
WORKDIR /src
RUN set -eu; if [ "$FUSE_PATCH" = 1 ]; then \
      (cd fs/fuse && patch -p1 < /patch/rekcod-fuse.patch && cp /patch/rekcod.c .); fi \
 && printf 'CONFIG_LOCALVERSION="-rekcod"\n# CONFIG_LOCALVERSION_AUTO is not set\n' >>rekcod.config \
 && ARCH=arm64 scripts/kconfig/merge_config.sh -m alpine.config rekcod.config >/dev/null \
 && make ARCH=arm64 olddefconfig >/dev/null \
 # merge_config only warns; a fragment line that didn't stick is a build error
 # (an "is not set" may vanish with its parent menu; it just must not be y/m)
 && bad=$( { grep -E '^CONFIG_[A-Z0-9_]+=' rekcod.config | while read -r l; do \
               grep -qxF "$l" .config || echo "$l"; done; \
             sed -n 's/^# \(CONFIG_[A-Z0-9_]*\) is not set$/\1/p' rekcod.config | while read -r s; do \
               ! grep -qE "^$s=[ym]$" .config || echo "$s"; done; } ) \
 && { [ -z "$bad" ] || { echo "kernel config did not take:"; echo "$bad"; exit 1; }; }
RUN make ARCH=arm64 -j"$(nproc)" -s Image modules \
 && mkdir -p /kout \
 && make ARCH=arm64 -s modules_install INSTALL_MOD_PATH=/kout INSTALL_MOD_STRIP=1 \
 && rm -f /kout/lib/modules/*/build /kout/lib/modules/*/source \
 && cp arch/arm64/boot/Image /kout/Image && cp .config /kout/config

# --- Alpine's stock kernel ---------------------------------------------------
FROM rootfs AS alpine-kernel
ARG KERNEL_REPO
ARG FUSE_PATCH
RUN apk add --no-cache build-base xz ${KERNEL_REPO:+--repository "$KERNEL_REPO"} linux-virt \
 && V=$(apk list -I linux-virt 2>/dev/null | sed -E 's/^linux-virt-([^ ]+) .*/\1/') \
 && apk add --no-cache ${KERNEL_REPO:+--repository "$KERNEL_REPO"} "linux-virt-dev=$V"
RUN KREL=$(ls /lib/modules) && KV=${KREL%%-*} \
 && wget -qO /tmp/linux.tar.xz "https://cdn.kernel.org/pub/linux/kernel/v6.x/linux-$KV.tar.xz" \
 && mkdir /src && tar -xJf /tmp/linux.tar.xz -C /src --strip-components=2 "linux-$KV/fs/fuse" \
 && rm /tmp/linux.tar.xz
COPY fuse/ /patch/
RUN set -eu; KREL=$(ls /lib/modules); D="/lib/modules/$KREL/kernel/fs/fuse"; \
    if [ "$FUSE_PATCH" = 1 ]; then cd /src/fuse \
      && patch -p1 < /patch/rekcod-fuse.patch && cp /patch/rekcod.c . \
      && make -s -C "/usr/src/linux-headers-$KREL" M="$PWD" modules \
      && for m in fuse virtiofs cuse; do gzip -9 -c "$m.ko" > "$D/$m.ko.gz"; done \
      && depmod -a "$KREL"; fi; \
    # the initramfs autoloads virtiofs for the share device; it must carry ours
    mkinitfs -o /boot/initramfs-virt "$KREL"; \
    # Alpine ships an EFI zboot vmlinuz; Virtualization.framework wants the raw arm64 Image.
    mkdir -p /kout/lib; k=/boot/vmlinuz-virt; \
    off=$(od -An -t u4 -j 8 -N 4 "$k" | tr -d ' '); len=$(od -An -t u4 -j 12 -N 4 "$k" | tr -d ' '); \
    tail -c +$((off + 1)) "$k" | head -c "$len" | gunzip > /kout/Image; \
    cp /boot/initramfs-virt /kout/initramfs; cp /boot/config-*-virt /kout/config; \
    cp -a /lib/modules /kout/lib/

FROM ${KERNEL}-kernel AS kernel

FROM alpine:${ALPINE} AS pack
RUN apk add --no-cache e2fsprogs
COPY --from=rootfs / /rootfs
COPY --from=kernel /kout/ /kout/
RUN set -eu; mkdir /out; rm -rf /rootfs/boot/* /rootfs/lib/modules; \
    cp -a /kout/lib/modules /rootfs/lib/; cp /kout/Image /out/Image; \
    [ "$(dd if=/out/Image bs=1 skip=56 count=4 2>/dev/null)" = "ARMd" ]; \
    [ ! -f /kout/initramfs ] || cp /kout/initramfs /out/initramfs; \
    # content + 25% + 64 MB: the root only takes logs and /etc writes; docker
    # state is on the data disk. The release tarball compresses the rest away.
    mb=$(( $(du -sm /rootfs | cut -f1) * 5 / 4 + 64 )); \
    mke2fs -q -t ext4 -L rekcod-root -d /rootfs /out/root.img "${mb}M"; \
    cat /out/Image $( [ -f /out/initramfs ] && echo /out/initramfs ) /out/root.img | sha256sum | cut -c1-16 > /out/VERSION

FROM scratch AS out
COPY --from=pack /out/ /
