#!/bin/sh
# Take ownership of the data volume, then drop privileges and run Tor.
#
# Docker creates a named volume owned by root whatever the image says, so this
# cannot be done at build time. Tor refuses to start on a DataDirectory it does
# not own, and npanel cannot read the control cookie unless the directory is
# traversable by its group.
set -eu

chown npanel-tor:npanel /var/lib/tor
chmod 0750 /var/lib/tor

exec su-exec npanel-tor tor -f /etc/tor/torrc "$@"
