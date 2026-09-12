FROM skyre-owned-linux-x11:20260907
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
    libpulse-dev pulseaudio pulseaudio-utils pkg-config python3 \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --uid 17072 skyre-audio
RUN rustup component add clippy
ENV CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=2
