# syntax=docker/dockerfile:1
# The Sheltie control plane. Build from the repository root:
#   docker build -f deploy/Dockerfile -t sheltie .
# Configure with environment variables (see docs/self-hosting.md); listens on SHELTIE_BIND.

FROM rust:1.97.0-bookworm AS build
# Parallel compile jobs; unset uses every CPU. Set it on a host shared with other services.
ARG BUILD_JOBS
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p sheltie-server ${BUILD_JOBS:+--jobs "$BUILD_JOBS"} \
    && install -D target/release/sheltie-server /out/sheltie-server

FROM gcr.io/distroless/cc-debian12:nonroot
LABEL org.opencontainers.image.title="sheltie-server" \
      org.opencontainers.image.description="Parallel merge engine for agent-generated pull requests" \
      org.opencontainers.image.source="https://github.com/softberries/sheltie" \
      org.opencontainers.image.licenses="Apache-2.0"
COPY --from=build /out/sheltie-server /usr/local/bin/sheltie-server
# Readable by any user the container runs as (compose runs it as the key file's owner).
WORKDIR /
ENV SHELTIE_BIND=0.0.0.0:8080
EXPOSE 8080
USER nonroot
HEALTHCHECK --interval=15s --timeout=5s --start-period=20s CMD ["/usr/local/bin/sheltie-server", "healthcheck"]
ENTRYPOINT ["/usr/local/bin/sheltie-server"]
CMD ["serve"]
