#!/bin/sh
# After the files are gone, for both packages. The npanel user and
# /var/lib/npanel stay: the database is the one thing a reinstall can't get
# back.
set -e

if [ -d /run/systemd/system ]; then
    systemctl daemon-reload
fi
