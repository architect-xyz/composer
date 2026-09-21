# Docker-based setup

Composer can run as a Docker Compose service alongside your other services.
This requires no host installation — just add it to your compose file.

For the host-native alternative (simpler config, no volume mounts), see
the main [README](../README.md) and [install guide](install.md).

## Running composer as a Docker Compose service

Add `afintech/composer:latest` as a service in your compose file:

```yaml
services:
  composer:
    image: afintech/composer:latest
    restart: unless-stopped
    environment:
      - RUST_LOG=composer=debug
      - COMPOSE_PROJECT_NAME=myproject
      - COMPOSE_PROJECT_DIRECTORY=${PWD}
      - WATCH_COMPOSE_FILE=true
      - COMPOSE_RUN_LOGS=/var/log/composer
      - COMPOSER_STATE_DIR=/var/lib/composer
      - PRUNE_IMAGES=0 0 7 * * *
    volumes:
      - /var/run/docker.sock:/var/run/docker.sock:ro
      - ./compose.yml:/compose.yml:ro
      - ./.env:/.env:ro
      # env files must also be mounted at the host project directory path
      # because docker compose resolves env_file: paths relative to it
      - ./.env:${PWD}/.env:ro
      - ./log/composer:/var/log/composer:rw
      # last-run records for `status`; without a mount they are lost
      # whenever the container is recreated
      - ./var/composer:/var/lib/composer:rw
    logging:
      driver: local

  # your other services...
```

### Why this is complex

When composer runs inside a container:

- The Docker socket must be volume-mounted
- The compose file must be mounted at a known path
- Environment files must be mounted at **two** paths: the container root
  (for composer's `--env-file`) and the host project directory (for
  `docker compose`'s `env_file:` resolution)
- `COMPOSE_PROJECT_DIRECTORY` must be set to the host path
- Log directories need a volume mount
- The state directory needs a volume mount (see below)
- System monitoring requires `pid: host`, `privileged: true`, and
  `network_mode: host`

The host-native binary avoids all of this.

### Last-run records

Scheduled jobs run with `--rm` and leave no container behind, so the
scheduler records when it last ran each service in `last-runs.json` and
`status` reads it back (see [Service status](../README.md#service-status)).
Inside a container the default location is `/root/.local/state/composer`,
in the container's writable layer: it survives a `restart` but not a
recreate (`docker compose up -d` after pulling a new image, `down`/`up`),
after which every job shows `never` until it next runs. Set
`COMPOSER_STATE_DIR` and mount a host directory there, as above, to keep it.

Records are keyed by `COMPOSE_PROJECT_DIRECTORY` plus the compose file's
name rather than by the container's mount path (`/compose.yml`), i.e. by
where the compose file lives on the host. So `composer status` run on the
host sees the container scheduler's records too, provided it reads the same
directory:

```bash
COMPOSER_STATE_DIR=./var/composer composer status -f compose.yml
```

This relies on the compose file being mounted under its own name
(`./compose.yml:/compose.yml`) and on `COMPOSE_PROJECT_DIRECTORY` being the
directory that contains it. The `/status.txt` endpoint is served by the
scheduler itself and needs none of this.

### Host system monitoring from Docker

To monitor host system metrics from inside the container, the container
needs elevated privileges:

```yaml
composer:
  image: afintech/composer:latest
  pid: host
  ipc: host
  privileged: true
  network_mode: host
  environment:
    - SYSTEM_MONITOR=true
  volumes:
    - /var/run/docker.sock:/var/run/docker.sock:ro
    - ./compose.yml:/compose.yml:ro
```

With the host-native binary, none of these extra permissions are needed.

### Installing aliases via Docker

If you don't have the binary installed:

```bash
docker run --rm -it \
  -v $HOME:/home -e HOME="/home" \
  --user $(id -u):$(id -g) \
  afintech/composer:latest install bash
```

## Switching to host-native

1. Install the binary: `curl -fsSL ... -o /usr/local/bin/composer`
2. Install the systemd unit: `composer install systemd --user ec2-user`
3. Remove the `composer` service from your compose file
4. Enable the systemd service: `systemctl enable --now composer`

The compose file goes from this:

```yaml
services:
  composer:
    image: afintech/composer:latest
    volumes: [...]
    environment: [...]

  grafana:
    image: grafana/grafana:12.3.1
```

To this:

```yaml
services:
  grafana:
    image: grafana/grafana:12.3.1
```

Composer runs on the host as a systemd service, reading the compose file
directly from the working directory.
