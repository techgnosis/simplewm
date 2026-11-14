#!/bin/sh
# In the container (distro/test-boot.sh runs it): put test/check.sh and test/bash_profile
# in a test image's /home/user.
set -eu
apk add -q e2fsprogs e2fsprogs-extra
img=$1
d=/work/distro/test
# A journal left by an unclean quit would overwrite debugfs's changes at boot.
e2fsck -fy "$img" >/dev/null 2>&1 || true
cat > /tmp/cmds <<C
cd /home/user
write $d/check.sh check.sh
write $d/bash_profile .bash_profile
sif check.sh uid 1000
sif check.sh gid 1000
sif .bash_profile uid 1000
sif .bash_profile gid 1000
C
debugfs -w -f /tmp/cmds "$img" >/dev/null
