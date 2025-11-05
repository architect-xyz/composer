<img src="LOGO.png" alt="composer logo" width="33%" />

# composer

Run or restart docker-compose services on a cron schedule.  This composer can itself be run as a docker-compose service--see `compose.yml` for an example.

Composer also provides some extra utilities you may find helpful to your docker-compose practice:

- Collect and push host metrics such as CPU, memory, and disk usage to OpenTelementry
- Monitor the status of docker-compose services and push change notifications to Slack
- Prune unused docker images periodically

All composer functionality is opt-in and controlled by flags or environment variables.

## Usage

Add `afintech/composer:latest` as a service to your docker compose file.  Generally, the
following configuration is what you want:

```
services:
  # ...
  scheduler:
    image: "afintech/composer:latest"
    environment:
      # recommended to set log level to debug
      - RUST_LOG=composer=debug
      # set a compose project name if this compose file doesn't already define one
      - COMPOSE_PROJECT_NAME=composer
      # ensure that docker compose working directory inside this container matches the host
      - COMPOSE_PROJECT_DIRECTORY=${PWD}
      # optionally watch the compose file for changes
      - WATCH_COMPOSE_FILE=true
    volumes:
      - /var/run/docker.sock:/var/run/docker.sock:ro
      # mount in the docker compose configuration
      - ./compose.yml:/compose.yml:ro
      # mount in the env file used by docker compose (optional)
      - ./.env:/.env:ro
      # you may also need to mount the env file into the compose --project-directory
      - ./.env:${PWD}/.env:ro
```

The scheduler uses container labels to determine when to run or restart them.  For example, the following configures the `foo` service to restart every day at 10:00.

```
foo:
    container_name: foo_container
    command: >-
        /app/foo.sh
    labels:
        - "co.architect.composer.restart=0 0 10 * * *"
```

and the following configures the `foo` service to run every day at 10:00.

```
foo:
    container_name: foo_container
    command: >-
        /app/foo.sh
    labels:
        - "co.architect.composer.run=0 0 10 * * *"
```

### Installing aliases

```
docker run --rm -it -v $HOME:/home -e HOME="/home" --user $(id -u):$(id -g) afintech/composer:latest install bash
```

## Automatic Docker image pruning

Composer can automatically prune unused Docker images on a schedule to free up disk space. This runs the `docker image prune -f` command at the specified interval.

To enable image pruning, set the `PRUNE_IMAGES` environment variable with a cron expression:

```yaml
scheduler:
  image: "afintech/composer:latest"
  environment:
    - PRUNE_IMAGES=0 0 2 * * *  # Run at 2:00 AM every day
  volumes:
    - /var/run/docker.sock:/var/run/docker.sock:ro
    - ./compose.yml:/compose.yml:ro
```

Alternatively, you can use the `--prune-images` CLI argument:

```bash
composer --prune-images "0 0 2 * * *" -f compose.yml
```

Like scheduled services, image pruning supports Slack notifications via the `SLACK_WEBHOOK_URL` and `SLACK_WEBHOOK_ON_ERROR_URL` environment variables.

## Host system monitoring from inside Docker

Composer can also monitor and alert on host system metrics (CPU, memory, disk).  To do this, the container must have privileged access to the host.  Use the following service config to ensure the correct access:

```
composer:
  # ...
  pid: host
  ipc: host
  privileged: true
  network_mode: host
```

### Sending host metrics to OpenTelemetry

Set the following environment variables to push host metrics (CPU, memory, disk usage) to an OpenTelemetry collector:

- `OTEL_EXPORTER_OTLP_ENDPOINT`: OTEL collector endpoint (required)
- `OTEL_EXPORTER_OTLP_HEADERS`: `key1=value1,key2=value2` OTEL collector HTTP headers (optional)
- `OTEL_METRIC_EXPORT_INTERVAL`: batch time in milliseconds (optional, default=5000)

The following metrics will be pushed:

- `memory.used_pct`: Percentage of memory currently in use.
- `memory.used_bytes`: Total bytes of memory currently in use.
- `memory.total_bytes`: Total bytes of memory available.
- `swap.used_pct`: Percentage of swap space currently in use.
- `swap.used_bytes`: Total bytes of swap space currently in use.
- `swap.total_bytes`: Total bytes of swap space available.
- `disk.used_pct`: Percentage of disk space currently in use (root disk only).
- `disk.used_bytes`: Total bytes of disk space currently in use (root disk only).
- `disk.total_bytes`: Total bytes of disk space available (root disk only).

Metrics will have the following scope attributes set:

- `host.name`: the configured `HOST` or hostname
- `service.name`: always `composer`

## Comparison to prior art

Compared to `ofelia` and `reddec/compose-scheduler`, the novel approach taken here is to leverage the Docker CLI itself to parse a compose configuration.  This allows us to use the simple labeling scheme without the shortcomings of only liaising with the Docker daemon.  This allows us to pick up compose file changes on the fly, run scheduled tasks that haven't been run for a first time, and restart compose services as if the host itself were restarting them.
