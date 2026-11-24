# Deploying wsp-hub from the image

The hub is one binary, `wsp-hub` (crates/hub). The `Dockerfile` at the repo root builds it in a Rust
builder stage (`cargo build --release --locked -p wsp-hub`) and copies it into a `debian:bookworm-slim`
image with CA certificates, running as the non-root user `wsp-hub` (uid 10001). `ENTRYPOINT` is
`wsp-hub`, `CMD` is `serve --config /etc/wsp-hub/hub.json`, the data volume is `/var/lib/wsp-hub` and the
port is 7489. Below, `<host>` is the name your DNS gives the server and `<name>` is the first admin.

## What is where

| In the container | What | From |
| --- | --- | --- |
| `/etc/wsp-hub/hub.json` | the config | `deploy/hub/hub.json`, mounted read-only |
| `/etc/wsp-hub/fullchain.pem`, `privkey.pem` | the certificate chain and its key (PEM) | mounted beside the config |
| `/var/lib/wsp-hub` | objects, `hub.db`, `api.token`, `api.log` | a named volume |

`hub.json` binds `0.0.0.0` on 7489 and admits every peer (`0.0.0.0/0`, `::/0`): who may do what is the
token a user logs in with, not the address. The hub refuses to start on a non-loopback address without
`tls`, so the certificate is not optional. The other keys (`name`, `lease_days`) are in
`crates/hub/src/config.rs`.

## Build

```sh
docker build -t wsp-hub:latest .
```

The `.dockerignore` keeps `target/`, `.git`, worktrees, databases and any `*.pem`/`*.key` out of the
build context. The image is about 90 MB (`docker image ls wsp-hub`).

## Certificate

**Supported: the hub terminates TLS itself.** A TLS-terminating proxy in front is *not* supported: the
hub speaks TLS only (every connection, once `tls` is set, is inside a TLS session) and has no plain-TCP
mode to sit behind a proxy on a public address. A proxy in TCP/SNI pass-through mode is fine, since it
never ends the TLS.

With a public name, take a Let's Encrypt certificate with certbot's standalone challenge (port 80 open
to the server for the challenge only, nothing else listening on it):

```sh
docker run --rm -p 80:80 -v certbot-etc:/etc/letsencrypt certbot/certbot \
  certonly --standalone -d <host> --agree-tos -m <your-email>
```

Then copy the pair into `deploy/hub/etc/`, which the compose file mounts:

```sh
mkdir -p deploy/hub/etc && cp deploy/hub/hub.json deploy/hub/etc/
docker run --rm -v certbot-etc:/le:ro -v "$PWD/deploy/hub/etc:/out" alpine sh -c \
  'cp -L /le/live/<host>/fullchain.pem /le/live/<host>/privkey.pem /out/ && chown 10001:10001 /out/*.pem && chmod 600 /out/privkey.pem'
```

The key must be readable by uid 10001. Certbot renews into the same volume: repeat the copy after a
renewal and `docker compose restart hub`, because the hub reads the files at start. (A cron line for
`certbot renew` and that copy keeps it current.)

With a private CA instead, put its server certificate chain and key in the same two files; each spoke
then passes the CA (see Logging in).

## First start

The first admin is made once, against the same volume, before the hub serves. It prints the admin's
token once; it is kept nowhere else, so save it now.

```sh
cd deploy/hub
docker compose run --rm hub admin init --config /etc/wsp-hub/hub.json --admin <name>
docker compose up -d
docker compose ps        # "healthy" after the first check
```

Without compose, the same two steps:

```sh
docker run --rm -v wsp-hub-data:/var/lib/wsp-hub -v "$PWD/etc:/etc/wsp-hub:ro" wsp-hub:latest \
  admin init --config /etc/wsp-hub/hub.json --admin <name>
docker run -d --name wsp-hub --restart unless-stopped -p 7489:7489 \
  -v wsp-hub-data:/var/lib/wsp-hub -v "$PWD/etc:/etc/wsp-hub:ro" wsp-hub:latest
```

`admin init` is refused once the hub has an active admin. The compose health check is a TLS handshake
against the hub's own port (`openssl s_client`, in the image); the hub has no readiness verb.

## Firewall

Open TCP **7489** (the `port` of `hub.json`) inbound and nothing else for the hub. Port 80 is needed only
while certbot answers its challenge.

## Logging in from a spoke

```sh
wsp login tls://<host>:7489
```

The certificate must verify for `<host>`, so use the name the certificate was issued for, not an IP
address. A certificate from a private CA needs that CA on the spoke's side.

## Backup

Everything the hub keeps is in the data volume. Stop the hub for a consistent copy (`hub.db` is SQLite):

```sh
docker compose stop hub
docker run --rm -v deploy-hub_hub-data:/data:ro -v "$PWD:/backup" alpine \
  tar czf /backup/wsp-hub-$(date +%F).tgz -C /data .
docker compose start hub
```

The volume's real name is `<compose project>_hub-data`; `docker volume ls` shows it. Restore into an
empty volume with `tar xzf` the other way round. Back up the certificate files separately, or just
issue a new one. Do not copy the `*.tgz` anywhere untrusted: it holds `api.token`.

## Upgrade

```sh
git pull
docker compose build hub         # or: docker build -t wsp-hub:latest .
docker compose up -d hub         # recreates the container on the new image; the volume stays
```

The data directory is read by the new binary as it stands; take a backup first. To go back, rebuild the
earlier commit and `up -d` again.

## Check a build

`deploy/hub/smoke.sh` builds the image (or takes `IMAGE=<tag>`), then on loopback with a throwaway CA:
runs `admin init`, checks that a second one is refused, serves over TLS, verifies the handshake against
the CA by name, and runs the health check command inside the container. It removes its container, volume
and temporary files.
