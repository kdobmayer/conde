# syntax=docker/dockerfile:1
# conde server image: release build on a non-root distroless runtime
# (glibc, libgcc, CA bundle for sslmode=verify-full; no shell).
# Build for linux/amd64 (ACR cannot parse --platform on a digest-pinned FROM, so it goes on the command line):
#   az acr build --platform linux/amd64 -r crnative -t conde:<short-sha> -f Dockerfile .

FROM rust:1.98-bookworm@sha256:93ce27a88655056a51dbdd8f5f2d7ddc071c7b0070fb288a37b5a285fc83971e AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
# setup.rs embeds the agent skills (include_str!).
COPY skills ./skills
COPY README.md ./README.md
RUN cargo build --release --locked -p conde --bin conde

# debian12 distroless no longer receives updates; a bookworm (glibc 2.36) binary runs on trixie.
FROM gcr.io/distroless/cc-debian13:nonroot@sha256:e792ab3d241a468a4fd7519ddbbebe66b49b5f365771716ea688ad40b6c6f1c2
COPY --from=build /src/target/release/conde /usr/local/bin/conde
USER 65532:65532
ENV PORT=8080
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/conde"]
CMD ["server"]
