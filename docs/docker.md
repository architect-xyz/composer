---
layout: default
title: Docker Setup
nav_order: 4
---

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
      - PRUNE_IMAGES=0 0 7 * * *
    volumes:
      - /var/run/docker.sock:/var/run/docker.sock:ro
      - ./compose.yml:/compose.yml:ro
      - ./.env:/.env:ro
      # env files must also be mounted at the host project directory path
      # because docker compose resolves env_file: paths relative to it
      - ./.env:${PWD}/.env:ro
      - ./log/composer:/var/log/composer:rw
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
- System monitoring requires `pid: host`, `privileged: true`, and
  `network_mode: host`

The host-native binary avoids all of this.

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
