# composer

Run or restart docker-compose services on a cron schedule.  This composer can itself be run as a docker-compose service--see `compose.yml` for an example.

Compared to `ofelia` and `reddec/compose-scheduler`, the novel approach taken here is to leverage the Docker CLI itself to parse a compose configuration.  This allows us to use the simple labeling scheme without the shortcomings of only liaising with the Docker daemon.  This allows us to pick up compose file changes on the fly, run scheduled tasks that haven't been run for a first time, and restart compose services as if the host itself were restarting them.

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
    volumes:
      - /var/run/docker.sock:/var/run/docker.sock:ro
      # mount in the docker compose configuration
      - ./compose.yml:/compose.yml:ro
      # mount in the env file used by docker compose (optional)
      - ./.env:/.env:ro
      # you may also need to mount the env file into the compose --project-directory
      - ./.env:${PWD}/.env:ro
```

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