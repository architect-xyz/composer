# Shell aliases reference

Composer ships a canonical set of shell aliases for common Docker Compose
operations. The same file works in both bash and zsh.

## Installation

```bash
composer install bash   # → ~/.bashrc.d/composer.bash
composer install zsh    # → ~/.zshrc.d/composer.zsh
```

Add `--with-guards` on either to also install [guards](#guards) — a shadow
`docker` alias and interactive `y/N` confirmation on destructive commands.

## Commands

### Inspect

| Command | Description |
|---------|-------------|
| `status` | Show composer daemon status via `http://localhost:10080/status.txt` |
| `status <svc>` | Show `docker compose ps` for a specific service |
| `logs <svc>` | Tail last 100 lines and follow |
| `logs -n 500 <svc>` | Tail last 500 lines and follow |

### Control

| Command | Description |
|---------|-------------|
| `start <svc>` | Start a stopped service |
| `stop <svc>` | Stop a running service |
| `restart <svc>` | Restart a service |

### Deploy

| Command | Description |
|---------|-------------|
| `up <svc...>` | Bring up one or more services (`docker compose up -d`) |
| `up -a` | Bring up all services |
| `down <svc...>` | Tear down one or more services |
| `down -a` | Tear down all services |
| `down -v <svc>` | Tear down and remove volumes |
| `upgrade <svc...>` | Pull latest image(s) for one or more services |
| `upgrade -a` | Pull latest images for all services |
| `upgrade --now <svc...>` | Pull, tear down, and bring back up |
| `upgrade -a --now` | Pull, tear down, and bring back up all services |

### Exec

| Command | Description |
|---------|-------------|
| `run <svc> [args...]` | `docker compose run --rm <svc> [args]` |
| `exec <svc> [args...]` | `docker compose exec <svc> [args]` |

## Flags

### `--profile <name>`

Accepted on all commands. Forwarded as `docker compose --profile <name>`.

```bash
up --profile monitoring -a
down --profile monitoring -a
upgrade --profile monitoring --now grafana
```

### `-a` / `--all`

Used with `up`, `down`, and `upgrade` to target all services instead of naming them.

### `-v`

Used with `down` to also remove volumes (`docker compose down -v`).

### `--now`

Used with `upgrade` to pull + down + up in one step, rather than just pulling.

### `-n <lines>`

Used with `logs` to set the number of tail lines (default: 100).

## `COMPOSE_PROJECT_DIR`

All commands go through a `_dc()` wrapper:

```bash
_dc() {
    docker compose ${COMPOSE_PROJECT_DIR:+--project-directory "$COMPOSE_PROJECT_DIR"} "$@"
}
```

If `COMPOSE_PROJECT_DIR` is set, all `docker compose` calls include
`--project-directory`. This is the integration point for project subshells.

### Usage with project subshells

In your project's subshell-env script:

```bash
export COMPOSE_PROJECT_DIR="/path/to/project"
source ~/.bashrc.d/composer.bash
```

If `COMPOSE_PROJECT_DIR` is not set (e.g. when SSH'd directly to an EC2
instance), `_dc()` is just `docker compose` running in the current directory.

## Guards

Guards are an opt-in hardening layer for shared or production hosts. Install
them with:

```bash
composer install bash --with-guards
composer install zsh  --with-guards
```

This appends a small snippet to the installed aliases file that:

- Exports `COMPOSER_GUARDS=1`, which makes destructive commands (`stop`,
  `up`, `down`, `upgrade --now`) print a `y/N` confirmation before executing.
- Aliases `docker` to a message steering users toward the wrapper commands,
  so habitual `docker compose ...` invocations don't bypass the wrappers.

To disable guards in the current shell without reinstalling:

```bash
unset COMPOSER_GUARDS
unalias docker
```

## Help banner

When the aliases file is sourced, it prints a help banner:

```
Available commands:
  Inspect ── status [svc] · logs [-n lines] <svc>
  Control ── start|stop|restart <svc>
  Deploy  ── up|down [-a] [-v] <svc...> · upgrade [-a] [--now] <svc...>
  Exec    ── run <svc> [args] · exec <svc> [args]
  All commands accept --profile <name>
```
