#!/bin/sh
# Stop dockerd cleanly (it stops containers), stop everything else, then
# unmount so the data disk is clean on the next boot.
pkill -TERM dockerd
i=0
while pgrep -x dockerd >/dev/null && [ $i -lt 180 ]; do sleep 0.1; i=$((i + 1)); done
killall5 -15
sleep 0.5
killall5 -9
sync
umount /Users /Volumes /private /var/folders /mnt/mac /mnt/rosetta 2>/dev/null
umount /var/lib/docker || mount -o remount,ro /var/lib/docker
mount -o remount,ro /
sync
