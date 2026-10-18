# Reproducible Linux build environment for Nullamp.
#   docker build -t nullamp-linux packaging/linux
#   docker run --rm -v "$PWD":/src -w /src nullamp-linux packaging/linux/build.sh
FROM rust:1.95-bookworm
RUN apt-get update && apt-get install -y --no-install-recommends \
        pkg-config libasound2-dev libxkbcommon-dev libwayland-dev \
        file desktop-file-utils squashfs-tools ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*
