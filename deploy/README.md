# Deploying Rustle

Two paths from a fresh Ubuntu or Debian VPS to a running instance. The Docker Compose
quickstart below is the fastest and is verified to take under 10 commands; the bare-metal
walkthrough after it is the same deployment with no container runtime, for a host that
already runs PostgreSQL and Caddy for other things.

Both need a domain name pointed at the VPS already — Caddy requests its certificate for
whatever `RUSTLE_DOMAIN` (or the Caddyfile itself, in the bare-metal path) says, and that
only succeeds once the DNS record resolves to this machine.

## Quickstart: Docker Compose

Run as a user in the `sudo` group, from an empty directory.

```sh
# 1. Install Docker.
curl -fsSL https://get.docker.com | sh

# 2. Get the source. docker-compose.yml builds the image itself; there is no published
#    one to pull instead.
git clone https://github.com/<you>/rustle.git && cd rustle

# 3. Configure. POSTGRES_PASSWORD, RUSTLE_BASE_URL and RUSTLE_DOMAIN are the only
#    required values — see .env.example for every optional one and its default.
cat > .env <<'EOF'
POSTGRES_PASSWORD=change-me-to-something-random
RUSTLE_BASE_URL=https://rss.example.com
RUSTLE_DOMAIN=rss.example.com
EOF

# 4. Build and start everything: PostgreSQL, Rustle, and Caddy in front of it.
sudo docker compose up -d --build

# 5. Rustle prints a one-time setup token on its very first start and never again —
#    this is what proves whoever opens /setup first is you, not a stranger who found
#    the site before you did.
sudo docker compose logs app | grep "setup token"
```

Open `https://rss.example.com/setup`, enter the token from step 5, and create your
account. Migrations run automatically on every start, so there is nothing to do by hand
with the database.

## Bare metal: systemd, Caddy, and `apt`'s own PostgreSQL

For a host where Docker is not wanted. Produces the same binary `docker-compose.yml`
does, by the same multi-stage `Dockerfile` — only the last stage (copying it out of the
image rather than running the image) differs.

### 1. PostgreSQL

```sh
sudo apt update && sudo apt install -y postgresql
sudo -u postgres createuser rustle
sudo -u postgres createdb --owner=rustle rustle
sudo -u postgres psql -c "alter user rustle with password 'change-me-to-something-random';"
```

### 2. The `rustle` system user

```sh
sudo useradd --system --no-create-home --shell /usr/sbin/nologin rustle
```

### 3. The binary

Built via the project's own `Dockerfile`, so the host needs only Docker for this one step
— not a Rust toolchain — and still ends up with a plain binary on its filesystem:

```sh
git clone https://github.com/<you>/rustle.git && cd rustle
DOCKER_BUILDKIT=1 docker build --target builder -t rustle-builder .
id=$(docker create rustle-builder)
docker cp "$id:/app/target/release/rustle" /usr/local/bin/rustle
docker rm "$id" >/dev/null
```

(Building with a locally installed Rust toolchain instead is just `cargo build --release`
— the release profile in `Cargo.toml` already sets `lto`, `strip` and `codegen-units = 1`.
Only skip the musl-specific packages from the `Dockerfile`'s builder stage if not
targeting musl.)

### 4. Configuration

```sh
sudo mkdir -p /etc/rustle
sudo tee /etc/rustle/rustle.env <<'EOF' >/dev/null
DATABASE_URL=postgres://rustle:change-me-to-something-random@localhost/rustle
RUSTLE_BASE_URL=https://rss.example.com
EOF
sudo chmod 600 /etc/rustle/rustle.env
```

Every other `RUSTLE_*` variable has the documented default from `.env.example` and only
needs adding here to override one.

### 5. The service

```sh
sudo cp deploy/rustle.service /etc/systemd/system/rustle.service
sudo systemctl daemon-reload
sudo systemctl enable --now rustle
```

Check it came up and grab the one-time setup token:

```sh
sudo journalctl -u rustle -n 50 --no-pager
```

### 6. Caddy

```sh
sudo apt install -y caddy
sudo cp deploy/Caddyfile /etc/caddy/Caddyfile
sudo sed -i 's/example.com/rss.example.com/' /etc/caddy/Caddyfile
sudo systemctl reload caddy
```

`deploy/Caddyfile`'s defaults (`RUSTLE_UPSTREAM` unset, so `127.0.0.1:8080`) already match
`rustle.service`'s address, so nothing else needs to change for this layout.

Open `https://rss.example.com/setup` and enter the setup token from step 5.

## Upgrading

Migrations are embedded in the binary and run automatically on startup — an upgrade is
only ever "replace the binary (or image), then restart":

```sh
# Docker Compose
git pull && sudo docker compose up -d --build

# Bare metal — rebuild per step 3, then:
sudo systemctl restart rustle
```

## Firewall

Only Caddy needs to be reachable from outside: `80` and `443`. Rustle's own
`RUSTLE_LISTEN_ADDR` default, `127.0.0.1:8080`, already refuses connections from anywhere
but Caddy on the same host; in the Docker Compose layout the app container publishes no
port at all; Caddy is the only one that does. `ufw allow 80,443/tcp` is enough on a bare
host that has nothing else listening.
