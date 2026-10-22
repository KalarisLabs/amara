FROM rust:1-slim-bookworm AS builder
WORKDIR /src
COPY . .
RUN cargo build --release --locked --bin amara

FROM debian:bookworm-slim
RUN useradd --create-home --uid 10001 amara
COPY --from=builder /src/target/release/amara /usr/local/bin/amara
USER amara
ENV AMARA_HOME=/home/amara/.amara
VOLUME ["/home/amara/.amara"]
# 7433/udp: LAN mesh (needs host networking for multicast) · 7432/tcp: HTTP API (token-protected)
EXPOSE 7433/udp 7432/tcp
ENTRYPOINT ["amara"]
CMD ["up", "--api", "0.0.0.0:7432"]
