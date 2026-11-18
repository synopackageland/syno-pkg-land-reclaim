# The image scripts/gate.sh runs the Rust, e2e and package gates in: CI's Rust
# image with its Debian packages (qemu, the armhf cross compiler, musl-tools),
# the three DSM targets, cargo-zigbuild and zig as the CLI's gate image has
# them, Node.js with pnpm, Go for e2e/ and Python for package/. gate.sh tags it
# with the first 12 hex digits of this file's SHA-256, so a change here builds
# a new image and an unchanged file reuses the one there.
FROM public.ecr.aws/docker/library/rust:1.97.1-bookworm@sha256:0e2bcaef56d041a486784e54104a81aebe0da44bd03019bd70bc0401e42e4a97
# e2e runs as uid 1000: the SDK runtime refuses to start the App as root.
RUN useradd --uid 1000 --no-create-home --home-dir /tmp gate
COPY --chmod=0755 <<'EOF' /usr/local/bin/debian-snapshot
#!/bin/sh
set -eu
rm -f /etc/apt/sources.list.d/debian.sources
echo "deb [check-valid-until=no] https://snapshot.debian.org/archive/debian/20260920T023840Z/ bookworm main" > /etc/apt/sources.list
apt-get -o Acquire::Retries=3 update
EOF
RUN debian-snapshot && apt-get install -y --no-install-recommends \
    qemu-user-static=1:7.2+dfsg-7+deb12u18+b3 \
    gcc-arm-linux-gnueabihf=4:12.2.0-3 \
    libc6-dev-armhf-cross=2.36-8cross1 libc6-armhf-cross=2.36-8cross1 \
    musl-tools=1.2.3-1 file python3 \
  && rm -rf /var/lib/apt/lists/*
RUN rustup component add clippy rustfmt \
  && rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl armv7-unknown-linux-gnueabihf
RUN curl -fsSL https://nodejs.org/dist/v24.22.1/node-v24.22.1-linux-x64.tar.xz | tar -xJ -C /usr/local --strip-components=1
RUN curl -fsSL https://go.dev/dl/go1.26.10.linux-amd64.tar.gz | tar -xz -C /usr/local \
  && ln -s /usr/local/go/bin/go /usr/local/go/bin/gofmt /usr/local/bin/
RUN curl -fsSL https://ziglang.org/download/0.16.0/zig-x86_64-linux-0.16.0.tar.xz | tar -xJ -C /opt \
  && ln -s /opt/zig-x86_64-linux-0.16.0/zig /usr/local/bin/zig
RUN cargo install --locked cargo-zigbuild@0.23.0 && rm -rf "$CARGO_HOME/registry" \
  && chmod -R a+w "$RUSTUP_HOME" "$CARGO_HOME"
# The registry npm installs pnpm from: a network that blocks registry.npmjs.org
# passes another; pnpm's version is pinned either way.
ARG NPM_REGISTRY=https://registry.npmjs.org
RUN npm install --global --registry "$NPM_REGISTRY" pnpm@10.33.2 && rm -rf /root/.npm
