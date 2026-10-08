#!/bin/sh
# After the files are in place, for both packages. dpkg calls this with
# `configure <old version>`, the old version empty on a first install; rpm with
# the number of copies installed, 1 on a first install.
set -e

case "$1" in
    configure) upgrade="${2:+yes}" ;;
    1) upgrade= ;;
    [0-9]*) upgrade=yes ;;
    *) exit 0 ;;
esac

systemd-sysusers npanel.conf
systemd-tmpfiles --create npanel.conf

# Without systemd running (a container, a chroot) there is nothing to tell.
[ -d /run/systemd/system ] || exit 0
systemctl daemon-reload

if [ -n "$upgrade" ]; then
    # Only if it was running: one stopped on purpose stays stopped.
    systemctl try-restart npanel.service
    exit 0
fi

# Not started, nor enabled: until DASHBOARD_HOST is set it would only fail and
# restart, every five seconds.
cat <<'EOF'
npanel is installed. Set DASHBOARD_HOST in /etc/npanel/.env, then:

    sudo systemctl enable --now npanel
    cd /etc/npanel && sudo -u npanel npanel make-admin npub1yourkeyhere

See docs/deployment.md for the reverse proxy, or for serving HTTPS without one.
EOF

# An npanel installed by hand before the package has its own unit, which
# overrides the package's and still runs /usr/local/bin/npanel.
if [ -e /etc/systemd/system/npanel.service ]; then
    cat <<'EOF'

/etc/systemd/system/npanel.service, from an install by hand, overrides the
unit this package installed and runs the old binary. Remove it, and
/usr/local/bin/npanel, then run `sudo systemctl daemon-reload && sudo systemctl
restart npanel`.
EOF
fi
