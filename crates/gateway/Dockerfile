# Multi-stage build for thclaws-gateway.
#
# Stage 1 (builder) compiles the release binary against Debian
# bookworm-slim with rustls (no native OpenSSL). Stage 2 ships a
# distroless cc image — small, no shell, no package manager.
#
# Build context is `crates/gateway/`. The crate is standalone (no
# workspace root), matching the line-server pattern.
#
# Build:
#   cd thclaws/crates/gateway
#   docker build -t thclaws-gateway:dev .
#
# Run:
#   docker run --rm -p 8080:8080 \
#       -e GOOGLE_CLIENT_ID=...apps.googleusercontent.com \
#       -e OPENAI_API_KEY=sk-... \
#       -e ANTHROPIC_API_KEY=sk-ant-... \
#       thclaws-gateway:dev

FROM rust:1.85-bookworm AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock* ./
COPY src ./src
RUN cargo build --release

FROM gcr.io/distroless/cc-debian12:nonroot
WORKDIR /app
COPY --from=builder /build/target/release/thclaws-gateway /app/thclaws-gateway
EXPOSE 8080
USER nonroot
ENTRYPOINT ["/app/thclaws-gateway"]
