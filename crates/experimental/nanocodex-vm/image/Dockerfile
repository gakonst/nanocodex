# Build context: crates/experimental/nanocodex-vm/image.
# The Rust guest runtime supplies capture/input and starts the X11 session.
FROM alpine:3.21@sha256:48b0309ca019d89d40f670aa1bc06e426dc0931948452e8491e3d65087abc07d
RUN apk add --no-cache ca-certificates xvfb openbox xterm font-dejavu \
    && mkdir -p /app /workspace /run/nanocodex-desktop \
    && chmod 0700 /run/nanocodex-desktop
ENV DISPLAY=:0
WORKDIR /app
CMD ["/bin/sh"]
