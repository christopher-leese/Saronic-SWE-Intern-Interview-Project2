# Multi-stage build (BASE-2-07): a Rust builder, a test stage, and a minimal runtime image.
# The builder's Rust version matches rust-toolchain.toml, and both base images are pinned by
# digest (multi-platform), so every build uses the same images (BASE-0-08).

FROM rust:1.97.0-slim-bookworm@sha256:6d220bf85c74e842a79da63997af8d2e74455c0b8847d8bb3a5888572334991d AS build
WORKDIR /app
COPY . .
RUN cargo build --release --locked

# `docker build --target test -t fleet-sim-test .` then `docker run --rm fleet-sim-test`.
FROM build AS test
RUN cargo test --locked --no-run
CMD ["cargo", "test", "--locked", "--offline"]

# Default target: only the release binary, run as a non-root user.
FROM gcr.io/distroless/cc-debian12:nonroot@sha256:9dac0a79194e45a7da0158a9c6da57b217585af0786db3845d1f0ec1a0dd182f AS runtime
COPY --from=build /app/target/release/fleet-sim /usr/local/bin/fleet-sim
ENTRYPOINT ["/usr/local/bin/fleet-sim"]
