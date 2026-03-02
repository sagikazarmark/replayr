# syntax=docker/dockerfile:1
# check=skip=CopyIgnoredFile

FROM --platform=$BUILDPLATFORM tonistiigi/xx:1.9.0@sha256:c64defb9ed5a91eacb37f96ccc3d4cd72521c4bd18d5442905b95e2226b0e707 AS xx

FROM --platform=$BUILDPLATFORM rust:1.93.1-slim@sha256:c0a38f5662afdb298898da1d70b909af4bda4e0acff2dc52aea6360a9b9c6956 AS base

RUN cargo install cargo-chef

COPY --from=xx / /

WORKDIR /usr/src/app


FROM base AS deps

COPY . .

RUN cargo chef prepare --recipe-path recipe.json


FROM base AS builder

RUN apt-get update && apt-get install -y clang lld

ARG TARGETPLATFORM

RUN xx-apt-get update && \
    xx-apt-get install -y \
    gcc \
    g++ \
    libc6-dev \
    pkg-config

RUN xx-cargo --setup-target-triple

COPY --from=deps /usr/src/app/recipe.json recipe.json

RUN xx-cargo chef cook --release --recipe-path recipe.json

COPY . .

RUN xx-cargo build --release --bin replayr
RUN xx-verify ./target/$(xx-cargo --print-target-triple)/release/replayr
RUN cp -r ./target/$(xx-cargo --print-target-triple)/release/replayr /usr/local/bin/replayr


FROM debian:13.3-slim@sha256:1d3c811171a08a5adaa4a163fbafd96b61b87aa871bbc7aa15431ac275d3d430

COPY --from=builder /usr/local/bin/replayr /usr/local/bin/

ENV RUST_LOG=info

EXPOSE 9090 9091

ENTRYPOINT ["replayr"]
CMD ["--help"]
