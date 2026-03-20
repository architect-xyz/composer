# Canonical Shell Aliases

## Problem

Three copies of composer shell aliases exist across repos, each slightly
different:

- `composer/src/aliases.bash` — oldest, simplest, has confirmation prompts
  and `alias docker=...` that later versions removed
- `ax/scripts/composer.zsh` + `ax/templates/composer.bash` — richest,
  with `--profile`, `_dc()` wrapper, `logs -n`, `down -v`, `upgrade --now`
- `brokerage/scripts/composer.bash` — near-identical copy of the ax zsh
  version with a different variable name

The mature versions (ax zsh, brokerage bash) converged to the same feature
set. The built-in `aliases.bash` that `composer install bash` ships is the
least capable. Projects shouldn't need to maintain their own copies.

## Goal

Ship one canonical alias file in composer that replaces all three. After
this, ax and brokerage delete their copies and source the one composer
installs.

## Commands

```
status [<svc>]              Show composer status, or ps for a specific service
logs [-n lines] <svc>       Tail logs (default 100 lines)
start <svc>                 Start a stopped service
stop <svc>                  Stop a service
restart <svc>               Restart a service
up [--profile P] [-a] <svc...>
                            Bring up services; -a for all
down [--profile P] [-a] [-v] [<svc>]
                            Tear down services; -a for all, -v to remove volumes
upgrade [--profile P] [--now] <svc>
                            Pull latest image; --now to also restart
run [--profile P] <svc> [args...]
                            docker compose run --rm
exec <svc> [args...]        docker compose exec
```

### Flag conventions

- `-a` / `--all` instead of a literal `"all"` positional — avoids collision
  with a service actually named "all".
- `--profile <name>` accepted on commands that pass through to
  `docker compose` (up, down, upgrade, run). Parsed and forwarded as
  `docker compose --profile <name> ...`.
- `-v` on `down` forwards to `docker compose down -v`.
- `--now` on `upgrade` means pull + down + up (not just pull).
- `-n <lines>` on `logs` sets tail length (default 100).

### Behavior decisions

- **No confirmation prompts.** Both mature versions removed them. Users
  operating in a subshell already know what they're doing.
- **No `alias docker=...`.** Both mature versions dropped it.
- **No `build`.** It's `up --build -d` — niche enough to type out.
- **No `undock`.** Project-specific; calls a script that lives in the
  project repo, not in composer.
- **No `up only`.** Niche profile trick. Projects that need it can add
  a local extension.
- **`status` uses curl + `docker compose ps`.** No docker-run-of-composer
  fallback — irrelevant once composer is a host daemon.
- **Help banner printed on source.** Both mature versions have this; good
  UX for interactive subshells.

### `_dc()` wrapper

All commands go through a `_dc()` helper:

```bash
_dc() {
    docker compose ${COMPOSE_PROJECT_DIR:+--project-directory "$COMPOSE_PROJECT_DIR"} "$@"
}
```

- If `COMPOSE_PROJECT_DIR` is set (subshell pattern via `just env`), it
  passes `--project-directory`.
- If unset (EC2 SSH, host daemon CWD), it's a plain `docker compose`.
- Projects set `COMPOSE_PROJECT_DIR` in their subshell-env scripts before
  sourcing the aliases. That's the only integration point.

## File changes

### `composer/src/aliases.bash` — rewrite

Replace the current 106-line file with the canonical version (~130 lines).
Single file, bash-compatible (works in both bash and zsh).

### `composer/src/install_commands.rs` — add zsh target

Currently only installs to `~/.bashrc.d/composer.bash`. Add:

- `composer install bash` — writes to `~/.bashrc.d/composer.bash` (unchanged)
- `composer install zsh` — writes to `~/.zshrc.d/composer.zsh` (or a
  user-specified path). Same file contents; bash/zsh syntax is compatible
  for this subset of shell features.

### ax repo (separate PR)

- Delete `scripts/composer.zsh`
- Delete `templates/composer.bash`
- Update `scripts/subshell-env.zsh` to set `COMPOSE_PROJECT_DIR` and
  source the composer-installed aliases
- Update `scripts/setup-ec2-local.sh` to run `composer install bash`
  instead of copying `templates/composer.bash`
- Keep project-specific extras (like `undock`) in a separate sourced file
  if still needed

### brokerage repo (separate PR)

- Delete `scripts/composer.bash` and `scripts/composer.zsh`
- Update `scripts/subshell-env.bash` and `scripts/subshell-env.zsh` to
  set `COMPOSE_PROJECT_DIR` and source the composer-installed aliases
- Keep `scripts/db-commands.bash` / `.zsh` — those are project-specific

## Migration

1. Land the new aliases in composer, release
2. Update ax and brokerage to source from composer instead of their copies
3. Verify subshell workflow (`just env <env>`) still works in both repos
4. Delete the old alias files from ax and brokerage
