#!/bin/sh
# Copy your ~/.ssh (keys and known_hosts) into /home/user/.ssh on the distro's home
# partition, for git over SSH there. Run it on the host with sudo:
#
#   sudo distro/copy-ssh.sh
#
# The partition is found by its GPT name (distro-home), mounted on a temporary folder and
# unmounted afterwards. An existing .ssh there is kept as .ssh.old.<time>.
set -eu

if [ "$(id -u)" -ne 0 ]; then
    echo "run it with sudo" >&2
    exit 1
fi
src=$(getent passwd "${SUDO_USER:-root}" | cut -d: -f6)/.ssh
dev=/dev/disk/by-partlabel/distro-home
[ -d "$src" ] || { echo "no $src" >&2; exit 1; }
[ -e "$dev" ] || { echo "no distro-home partition" >&2; exit 1; }

mnt=$(mktemp -d)
mount "$dev" "$mnt"
trap 'umount "$mnt"; rmdir "$mnt"' EXIT
[ -d "$mnt/user" ] || { echo "no /home/user on the partition" >&2; exit 1; }
if [ -e "$mnt/user/.ssh" ]; then
    mv "$mnt/user/.ssh" "$mnt/user/.ssh.old.$(date +%s)"
    echo "kept the existing .ssh as .ssh.old.*"
fi
cp -a "$src" "$mnt/user/.ssh"
# ssh refuses keys others can read. The distro's user is UID 1000.
chmod 700 "$mnt/user/.ssh"
chmod -R go-rwx "$mnt/user/.ssh"
chown -R 1000:1000 "$mnt/user/.ssh"
ls -lan "$mnt/user/.ssh"
echo done
