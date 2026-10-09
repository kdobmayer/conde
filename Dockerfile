# syntax=docker/dockerfile:1
# conde server image: release build on a non-root distroless runtime
# (glibc, libgcc, CA bundle for sslmode=verify-full; no shell).
# Build for linux/amd64 (ACR cannot parse --platform on a digest-pinned FROM, so it goes on the command line):
#   az acr build --platform linux/amd64 -r crnative -t conde:<short-sha> -f Dockerfile .

# The web app (SvelteKit, static): built with node 22, embedded into the binary below.
FROM node:22-bookworm-slim@sha256:c3de60bf2f9dd0ac6370e6117950ff62d6e339527e7472301c9c78a017978392 AS web
WORKDIR /src/web
COPY web/package.json web/package-lock.json ./
RUN npm ci --no-audit --no-fund
COPY web ./
# writes /src/crates/conde-remote/web-dist
RUN npm run build

FROM rust:1.98-bookworm@sha256:93ce27a88655056a51dbdd8f5f2d7ddc071c7b0070fb288a37b5a285fc83971e AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
# setup.rs embeds the agent skills (include_str!); conde-remote embeds the web app (rust-embed).
COPY skills ./skills
COPY README.md ./README.md
COPY --from=web /src/crates/conde-remote/web-dist ./crates/conde-remote/web-dist
RUN cargo build --release --locked -p conde --bin conde

# debian12 distroless no longer receives updates; a bookworm (glibc 2.36) binary runs on trixie.
FROM gcr.io/distroless/cc-debian13:nonroot@sha256:e792ab3d241a468a4fd7519ddbbebe66b49b5f365771716ea688ad40b6c6f1c2
COPY --from=build /src/target/release/conde /usr/local/bin/conde
USER 65532:65532
ENV PORT=8080
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/conde"]
CMD ["server"]
