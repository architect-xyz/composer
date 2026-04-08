# Installation & deployment guide

## Installing the binary

The easiest way to install:

```bash
curl -fsSL https://raw.githubusercontent.com/architect-xyz/composer/main/install.sh | sh
```

This installs to `~/.local/bin` by default (no sudo required). To install
system-wide, pass `--to /usr/local/bin` (requires sudo):

```bash
curl -fsSL https://raw.githubusercontent.com/architect-xyz/composer/main/install.sh | sudo sh -s -- --to /usr/local/bin
```

Or download manually:

```bash
ARCH=$(uname -m)
case "$ARCH" in
  x86_64)  ARCH="amd64" ;;
  aarch64) ARCH="arm64" ;;
  arm64)   ARCH="arm64" ;;
esac
OS=$(uname -s | tr '[:upper:]' '[:lower:]')

mkdir -p ~/.local/bin
curl -fsSL "https://github.com/afintech/composer/releases/latest/download/composer-${OS}-${ARCH}" \
  -o ~/.local/bin/composer
chmod +x ~/.local/bin/composer
```

> **Note:** Ensure `~/.local/bin` is in your `PATH`. Add `export PATH="$HOME/.local/bin:$PATH"` to your shell profile if needed.

To pin a specific version, replace `latest` with the version tag (e.g. `v0.11.0`).

## Running as a systemd service (Linux)

```bash
# Install the systemd unit
sudo composer install systemd \
  --user ec2-user \
  --working-dir /home/ec2-user \
  --compose-file compose.yml

# Enable and start
sudo systemctl enable --now composer

# Check status
systemctl status composer
composer install status
```

### Options

| Flag | Default | Description |
|------|---------|-------------|
| `--user` | Current user | User to run the service as |
| `--working-dir` | User's home directory | Working directory (where compose.yml lives) |
| `--compose-file` | `compose.yml` | Compose file path relative to working dir |
| `--env` | — | Extra environment variables (repeatable, `KEY=VALUE`) |

### Generated unit file

The command writes to `/etc/systemd/system/composer.service` and runs
`systemctl daemon-reload`. It does **not** enable or start the service
automatically — this is intentional for EC2 user_data scripts where the
compose file may not exist yet.

### Adding extra environment variables

```bash
sudo composer install systemd \
  --user ec2-user \
  --working-dir /home/ec2-user \
  --env SLACK_WEBHOOK_URL=https://hooks.slack.com/... \
  --env PRUNE_IMAGES="0 0 2 * * *" \
  --env WATCH_COMPOSE_FILE=true
```

### Multiple projects on one host

Run one composer instance per project using systemd template units or
separate unit names:

```bash
# Project A
sudo composer install systemd \
  --user deploy \
  --working-dir /opt/project-a \
  --compose-file compose.yml

# For a second project, manually copy and edit the unit file:
sudo cp /etc/systemd/system/composer.service \
       /etc/systemd/system/composer-project-b.service
# Edit WorkingDirectory and ExecStart, then:
sudo systemctl daemon-reload
sudo systemctl enable --now composer-project-b
```

## Running as a launchd service (macOS)

```bash
# Install the launchd plist (from the project directory)
cd ~/my-project
composer install launchd

# Load the service
launchctl load ~/Library/LaunchAgents/com.architect.composer.plist
```

### Options

| Flag | Default | Description |
|------|---------|-------------|
| `--working-dir` | Current directory | Working directory |
| `--compose-file` | `compose.yml` | Compose file path relative to working dir |
| `--env` | — | Extra environment variables (repeatable, `KEY=VALUE`) |

Logs go to `/tmp/composer.log` and `/tmp/composer.err`.

## Shell aliases

```bash
composer install bash   # writes to ~/.bashrc.d/composer.bash
composer install zsh    # writes to ~/.zshrc.d/composer.zsh
```

Both install the same file — the syntax is bash/zsh compatible. The aliases
are sourced automatically if your shell is configured to source files from
`~/.bashrc.d/` or `~/.zshrc.d/`.

## Checking installation status

```bash
$ composer install status
composer v0.11.0
  binary: /usr/local/bin/composer
  bash aliases: /home/ec2-user/.bashrc.d/composer.bash
  zsh aliases: not installed
  systemd: /etc/systemd/system/composer.service
    user: ec2-user
    working dir: /home/ec2-user
    exec: /usr/local/bin/composer -f compose.yml
    state: active, enabled
```

## EC2 user_data bootstrap

A typical EC2 bootstrap script:

```bash
#!/bin/bash
set -euo pipefail

# Install composer (system-wide since this runs as root)
COMPOSER_VERSION="v0.11.0"
curl -fsSL https://raw.githubusercontent.com/architect-xyz/composer/main/install.sh \
  | sh -s -- --version "$COMPOSER_VERSION" --to /usr/local/bin

# Install shell aliases for interactive SSH sessions
sudo -u ec2-user composer install bash

# Install systemd unit (will start after configs are deployed)
composer install systemd \
  --user ec2-user \
  --working-dir /home/ec2-user \
  --env WATCH_COMPOSE_FILE=true \
  --env PRUNE_IMAGES="0 0 7 * * *"

# Don't enable yet — compose.yml hasn't been deployed.
# The deploy script will run: systemctl enable --now composer
```

## Upgrading

Use the built-in update command:

```bash
composer update
```

Or replace the binary manually and restart the service:

```bash
# Download new version (adjust path to match your install location)
curl -fsSL https://github.com/afintech/composer/releases/latest/download/composer-linux-amd64 \
  -o ~/.local/bin/composer
chmod +x ~/.local/bin/composer

# Restart
sudo systemctl restart composer
```
