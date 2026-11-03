# The Thingscan account server. Build from the repository root:
#   docker build -f deploy/Dockerfile -t thingscan-server .
FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p thingscan-server

FROM debian:bookworm-slim
RUN useradd --system --uid 10001 --home-dir /data thingscan \
    && mkdir -p /data && chown thingscan /data
COPY --from=build /src/target/release/thingscan-server /usr/local/bin/thingscan-server
USER thingscan
ENV THINGSCAN_DB=/data/thingscan.db \
    THINGSCAN_ADDR=0.0.0.0:8080
VOLUME /data
EXPOSE 8080
CMD ["thingscan-server"]
