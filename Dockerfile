# syntax=docker/dockerfile:1
# check=skip=CopyIgnoredFile

FROM --platform=$BUILDPLATFORM tonistiigi/xx:1.9.0@sha256:c64defb9ed5a91eacb37f96ccc3d4cd72521c4bd18d5442905b95e2226b0e707 AS xx

FROM --platform=$BUILDPLATFORM rust:1.93.1-slim@sha256:7e6fa79cf81be23fd45d857f75f583d80cfdbb11c91fa06180fd747fda37a61d AS base

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


FROM debian:13.5-slim@sha256:b6e2a152f22a40ff69d92cb397223c906017e1391a73c952b588e51af8883bf8

COPY --from=builder /usr/local/bin/replayr /usr/local/bin/

ENV RUST_LOG=info

EXPOSE 9090 9091

ENTRYPOINT ["replayr"]
CMD ["--help"]
