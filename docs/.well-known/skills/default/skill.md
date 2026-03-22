# Composer — Cron for Docker Compose

Composer is a scheduler that runs or restarts Docker Compose services on cron schedules. Schedules are defined as labels in `compose.yml`. Composer runs as a host-native daemon (systemd/launchd) or as a Docker container.

## Installation

```bash
# One-line install (Linux and macOS)
curl -fsSL https://raw.githubusercontent.com/architect-xyz/composer/main/install.sh | sh

# Then install as a system service:
sudo composer install systemd   # Linux
composer install launchd        # macOS
```

## Scheduling labels

Add labels to Docker Compose services:

| Label | Purpose |
|-------|---------|
| `co.architect.composer.run=<cron>` | Run the service on schedule |
| `co.architect.composer.restart=<cron>` | Restart the service on schedule |
| `co.architect.composer.tz=<timezone>` | Timezone (default: UTC) |
| `co.architect.composer.run=manual` | Register for status tracking only |
| `co.architect.composer.notify.slack=true` | Enable Slack notifications |
| `co.architect.composer.notify.slack.on-error=true` | Notify on errors only |

### Multiple schedules

Append a suffix to the label key:

```yaml
labels:
  - "co.architect.composer.run.morning=0 0 8 * * *"
  - "co.architect.composer.run.evening=0 0 18 * * *"
```

## Cron expression format

**IMPORTANT: Composer uses Quartz-compatible cron expressions with 6 fields (seconds first).** This is different from standard 5-field cron.

```
┌─── second (0-59)
│ ┌─── minute (0-59)
│ │ ┌─── hour (0-23)
│ │ │ ┌─── day of month (1-31)
│ │ │ │ ┌─── month (1-12 or JAN-DEC)
│ │ │ │ │ ┌─── day of week (0-6 or SUN-SAT)
│ │ │ │ │ │
* * * * * *
```

| Expression | Meaning |
|-----------|---------|
| `0 0 2 * * *` | Every day at 2:00 AM |
| `0 */15 * * * *` | Every 15 minutes |
| `0 0 6 * * MON-FRI` | Weekdays at 6:00 AM |
| `*/30 * * * * *` | Every 30 seconds |

## Common gotchas

- **6-field cron, not 5-field.** `0 2 * * *` (standard cron for 2 AM) is WRONG. Use `0 0 2 * * *` (seconds first).
- **Default timezone is UTC.** Set `co.architect.composer.tz` if you need local time.
- **`run` vs `restart`:** `run` executes `docker compose run --rm <service>` (starts a new container). `restart` executes `docker compose restart <service>` (restarts an existing running container).
- **Compose file auto-detection:** Composer looks for `compose.yml`, `compose.yaml`, `docker-compose.yml`, `docker-compose.yaml` in the working directory. Use `-f` to specify explicitly.

## Example compose.yml

```yaml
services:
  backup:
    image: my-backup:latest
    labels:
      - "co.architect.composer.run=0 0 2 * * *"
      - "co.architect.composer.tz=America/New_York"
      - "co.architect.composer.notify.slack.on-error=true"

  api:
    image: my-api:latest
    labels:
      - "co.architect.composer.restart.weekday=0 0 6 * * MON-FRI"
      - "co.architect.composer.restart.weekend=0 0 9 * * SAT,SUN"
```

## Environment variables

| Variable | Purpose |
|----------|---------|
| `SLACK_WEBHOOK_URL` | Slack webhook for all notifications |
| `SLACK_WEBHOOK_ON_ERROR_URL` | Slack webhook for errors only |
| `PRUNE_IMAGES` | Cron schedule for `docker image prune -f` |
| `COMPOSE_RUN_LOGS` | Directory to write job stdout/stderr logs |
| `WATCH_COMPOSE_FILE` | Set to `true` to auto-reload on compose file changes |

## Documentation

- Full README: https://github.com/architect-xyz/composer
- Installation guide: https://github.com/architect-xyz/composer/blob/main/docs/install.md
- Shell aliases: https://github.com/architect-xyz/composer/blob/main/docs/aliases.md
- Docker setup: https://github.com/architect-xyz/composer/blob/main/docs/docker.md
