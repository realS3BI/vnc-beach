# VNC Beach coordination server. Build from the repository root:
#   docker build -f crates/beach-coord/Dockerfile -t beach-coord .
FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p beach-coord

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --home-dir /data beach \
    && mkdir /data && chown beach /data
COPY --from=build /src/target/release/beach-coord /usr/local/bin/beach-coord
USER beach
ENV BEACH_DATA=/data BEACH_LISTEN=0.0.0.0:8080 RUST_LOG=info
VOLUME /data
EXPOSE 8080
ENTRYPOINT ["beach-coord"]
