FROM debian:bookworm-slim

RUN apt-get update \
 && apt-get install -y --no-install-recommends usbmuxd \
 && rm -rf /var/lib/apt/lists/* \
 && mkdir -p /var/lib/lockdown

HEALTHCHECK --interval=5s --timeout=2s --start-period=10s --retries=6 \
  CMD test -S /var/run/usbmuxd || exit 1

STOPSIGNAL SIGTERM

ENTRYPOINT ["usbmuxd", "-f"]
