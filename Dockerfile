# wsp-hub: the hub as a container image.
#
#   docker build -t wsp-hub .
#
# Run and deploy: docs/hub-deploy.md, deploy/hub/.

# ---- build ----
FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p wsp-hub \
 && strip target/release/wsp-hub

# ---- run ----
FROM debian:bookworm-slim
# ca-certificates: the hub's trust store for anything it dials. openssl: the
# compose health check is a TLS handshake (`openssl s_client`).
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates openssl \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --user-group --no-create-home --shell /usr/sbin/nologin wsp-hub \
 && mkdir -p /var/lib/wsp-hub /etc/wsp-hub \
 && chown wsp-hub:wsp-hub /var/lib/wsp-hub
COPY --from=build /src/target/release/wsp-hub /usr/local/bin/wsp-hub
USER wsp-hub
# data_dir of the example config: objects, hub.db, api.token, api.log.
VOLUME /var/lib/wsp-hub
# The hub's default port (crates/hub/src/config.rs DEFAULT_PORT).
EXPOSE 7489
ENTRYPOINT ["wsp-hub"]
CMD ["serve", "--config", "/etc/wsp-hub/hub.json"]
