# The Tor daemon npanel drives to publish an onion address for every site.
#
# Built here rather than pulled from a third party: it is Alpine's own tor
# package and a config file, which is less to audit than someone else's image
# and less to trust with the keys to every site this gateway serves.
#
# compose.yaml runs this in npanel's network namespace, so the control port
# below is reachable only from npanel and never appears on any network.

FROM alpine:3.22

RUN apk add --no-cache tor su-exec

# gid 1000 matches the `npanel` user npanel's image runs as. Tor writes its control
# cookie owned by its own group, so this is what makes the cookie readable in
# the other container.
RUN addgroup -g 1000 npanel && adduser -D -H -G npanel -u 1000 npanel-tor

COPY torrc /etc/tor/torrc
COPY entrypoint.sh /usr/local/bin/entrypoint.sh

EXPOSE 9051

ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
