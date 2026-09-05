FROM alpine:3.22

RUN apk add --no-cache \
      ca-certificates \
      chromium \
      font-noto \
      mesa-dri-gallium \
      nss-tools \
      socat \
      xvfb-run \
    && adduser -D browser \
    && install -d -o browser -g browser /home/browser/profile

COPY browser-vm-init /usr/local/bin/nanocodex-browser-vm-init

RUN chmod 0755 /usr/local/bin/nanocodex-browser-vm-init

WORKDIR /home/browser
