# Composer v2: Host-Native Daemon

## Problem

Composer currently runs as a Docker container (`afintech/composer:latest` on
`docker:27-cli` base). It manages other Docker Compose services by shelling out
to `docker compose` — but to do that from inside a container, it needs the Docker
socket mounted, the compose file mounted, env files mounted at multiple paths,
and a log directory mounted. A typical deployment looks like:

```yaml
services:
  composer:
    image: afintech/composer:latest
    volumes:
      - /var/run/docker.sock:/var/run/docker.sock:ro
      - ./compose.yml:/compose.yml:ro
      - ./.env:/.env:ro
      - ./.env.secret:/.env.secret:ro
      - ./.env:/home/ec2-user/.env:ro        # path gymnastics for compose
      - ./.env.secret:/home/ec2-user/.env.secret:ro
      - ./log/composer:/var/log/composer:rw
    environment:
      - COMPOSE_PROJECT_DIRECTORY=${PWD}      # because CWD is wrong inside the container
```

This is fragile and confusing. The env files need to be mounted at both `/` (for
composer's own `--env-file` flag) and at the host user's home directory (because
`docker compose` resolves `env_file:` paths relative to the project directory,
which is the host path). Adding system monitoring requires `pid: host`,
`privileged: true`, and `network_mode: host`. The Dockerfile exists solely to
put the binary next to a Docker CLI — but the host already has Docker.

**The irony: composer already just shells out to `docker compose`. There is no
reason it needs to live inside a container.**

## Vision

Composer becomes a host-native binary, installed and managed like Nomad or
Tailscale — `brew install`, a systemd unit, direct filesystem access. No volume
mounts, no path translation, no privileged containers.

### Before (container)

```
EC2 instance
├── Docker
│   ├── composer container (needs socket mount, file mounts, privileged...)
│   ├── app container
│   ├── postgres container
│   └── ...
└── compose.yml, .env, .env.secret (on host, mounted into composer)
```

### After (host daemon)

```
EC2 instance
├── composer (systemd service, runs as ec2-user)
│   └── reads compose.yml, .env, .env.secret directly from ~/
├── Docker
│   ├── app container
│   ├── postgres container
│   └── ...
└── compose.yml, .env, .env.secret (on host, read directly by composer)
```

### Before: ax grafana compose.yml

```yaml
services:
  composer:
    image: afintech/composer:latest
    restart: unless-stopped
    ports:
      - "127.0.0.1:10080:10080"
    environment:
      - RUST_LOG=composer=trace
      - COMPOSE_PROJECT_DIRECTORY=${PWD}
      - COMPOSE_RUN_LOGS=/var/log/composer
      - PRUNE_IMAGES=0 0 7 * * *
    env_file:
      - .env
    volumes:
      - /var/run/docker.sock:/var/run/docker.sock:ro
      - ./compose.yml:/compose.yml:ro
      - ./.env:/.env:ro
      - ./.env.secret:/.env.secret:ro
      - ./.env:/home/ec2-user/.env:ro
      - ./.env.secret:/home/ec2-user/.env.secret:ro
      - ./log/composer:/var/log/composer:rw
    logging:
      driver: local

  grafana:
    image: grafana/grafana:12.3.1
    # ...
```

### After: ax grafana compose.yml

```yaml
services:
  grafana:
    image: grafana/grafana:12.3.1
    # ...
```

Composer is no longer a Docker service. It runs on the host via systemd.

### Before: brokerage2 EC2 user_data (no composer)

```bash
# No scheduler, no status endpoint, no image pruning.
# Manual: scp configs, ssh, docker compose pull, docker compose up -d
```

### After: brokerage2 EC2 user_data

```bash
# Install composer
curl -fsSL https://github.com/afintech/composer/releases/latest/download/composer-linux-amd64 \
  -o /usr/local/bin/composer
chmod +x /usr/local/bin/composer

# Install shell aliases
composer install bash

# Install systemd unit (runs as ec2-user, working dir ~/)
composer install systemd --user ec2-user --working-dir /home/ec2-user
systemctl enable --now composer
```

That's it. Composer picks up `~/compose.yml` and starts scheduling,
monitoring, and serving status. Deploy becomes `scp configs && ssh
"just deploy"` with composer already running.

---

## Implementation Plan

### Phase 1: Build & Release Pipeline (release native binaries)

**Goal**: Produce downloadable binaries for linux/amd64, linux/arm64,
darwin/amd64, darwin/arm64 on every release.

**Changes to `.github/workflows/release.yml`:**

1. Add a `build-binaries` job that cross-compiles with `cross` (or Depot +
   `cargo-zigbuild`) for the four targets.
2. Create a GitHub Release (use `gh release create` or `softprops/action-gh-release`)
   with the four binaries attached, named:
   - `composer-linux-amd64`
   - `composer-linux-arm64`
   - `composer-darwin-amd64`
   - `composer-darwin-arm64`
3. Keep the existing Docker image build for backward compatibility, but it is
   no longer the primary distribution path.
4. Tag releases with semver (e.g., `v0.11.0`). Use `latest` download URL
   for convenience (`/releases/latest/download/composer-linux-amd64`).

**New file: `.github/workflows/release.yml` (rewrite)**

```yaml
name: Release
on:
  push:
    tags: ["v*"]

jobs:
  build-binaries:
    strategy:
      matrix:
        include:
          - target: x86_64-unknown-linux-musl
            artifact: composer-linux-amd64
          - target: aarch64-unknown-linux-musl
            artifact: composer-linux-arm64
          - target: x86_64-apple-darwin
            artifact: composer-darwin-amd64
          - target: aarch64-apple-darwin
            artifact: composer-darwin-arm64
    runs-on: ubuntu-latest  # use cross for all targets
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - name: Install cross
        run: cargo install cross --locked
      - name: Build
        run: cross build --release --target ${{ matrix.target }}
      - name: Rename artifact
        run: cp target/${{ matrix.target }}/release/composer ${{ matrix.artifact }}
      - uses: actions/upload-artifact@v4
        with:
          name: ${{ matrix.artifact }}
          path: ${{ matrix.artifact }}

  release:
    needs: build-binaries
    runs-on: ubuntu-latest
    steps:
      - uses: actions/download-artifact@v4
      - name: Create GitHub Release
        uses: softprops/action-gh-release@v2
        with:
          files: |
            composer-linux-amd64/composer-linux-amd64
            composer-linux-arm64/composer-linux-arm64
            composer-darwin-amd64/composer-darwin-amd64
            composer-darwin-arm64/composer-darwin-arm64

  docker:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: depot/setup-action@v1
      - uses: docker/login-action@v3
        with:
          username: ${{ secrets.DOCKERHUB_USERNAME }}
          password: ${{ secrets.DOCKERHUB_TOKEN }}
      - name: Build and push Docker image
        run: |
          depot build --push --project 2m5pzqgvpk \
            --platform "linux/arm64,linux/amd64" \
            -t afintech/composer:latest \
            -t afintech/composer:${{ github.ref_name }} .
        env:
          DEPOT_TOKEN: ${{ secrets.DEPOT_TOKEN }}
```

**Estimated scope**: ~1 file rewrite, ~50 lines. May need a `Cross.toml` for
musl target configuration.

---

### Phase 2: `composer install systemd` subcommand

**Goal**: `composer install systemd` generates and installs a systemd unit file
that runs composer as a host daemon.

**Changes to `src/install_commands.rs`:**

Add a `Systemd` variant to `InstallCommands`:

```rust
#[derive(Subcommand)]
pub enum InstallCommands {
    /// Install bash aliases to ~/.bashrc.d/composer.bash
    Bash,
    /// Install and enable a systemd service unit
    Systemd {
        /// User to run as (default: current user)
        #[clap(long, default_value_t = whoami::username())]
        user: String,
        /// Working directory (default: user's home)
        #[clap(long)]
        working_dir: Option<String>,
        /// Compose file path relative to working dir (default: compose.yml)
        #[clap(long, default_value = "compose.yml")]
        compose_file: String,
        /// Extra environment variables (KEY=VALUE), repeatable
        #[clap(long)]
        env: Vec<String>,
    },
}
```

Generated unit file (`/etc/systemd/system/composer.service`):

```ini
[Unit]
Description=Composer – Docker Compose scheduler
After=docker.service
Requires=docker.service

[Service]
Type=simple
User={user}
WorkingDirectory={working_dir}
ExecStart=/usr/local/bin/composer -f {compose_file}
Restart=on-failure
RestartSec=5s
Environment=RUST_LOG=composer=info

[Install]
WantedBy=multi-user.target
```

The subcommand:
1. Writes the unit file to `/etc/systemd/system/composer.service`
2. Runs `systemctl daemon-reload`
3. Prints instructions: `systemctl enable --now composer`

No `systemctl enable` by default — the caller decides when to start it (this
matters for user_data scripts where compose.yml hasn't been scp'd yet).

**Also add `composer install launchd`** for macOS (local dev):

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" ...>
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.architect.composer</string>
    <key>ProgramArguments</key>
    <array>
        <string>/opt/homebrew/bin/composer</string>
        <string>-f</string>
        <string>{compose_file}</string>
    </array>
    <key>WorkingDirectory</key>
    <string>{working_dir}</string>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>/tmp/composer.log</string>
    <key>StandardErrorPath</key>
    <string>/tmp/composer.err</string>
</dict>
</plist>
```

**Estimated scope**: ~120 lines added to `install_commands.rs`. Add `whoami`
crate dependency.

---

### Phase 3: Homebrew Formula

**Goal**: `brew install afintech/tap/composer` on macOS and Linux.

Create a Homebrew tap repo (`afintech/homebrew-tap`) with a formula:

```ruby
class Composer < Formula
  desc "Docker Compose scheduler and service monitor"
  homepage "https://github.com/afintech/composer"
  version "0.11.0"

  on_macos do
    on_arm do
      url "https://github.com/afintech/composer/releases/download/v0.11.0/composer-darwin-arm64"
      sha256 "..."
    end
    on_intel do
      url "https://github.com/afintech/composer/releases/download/v0.11.0/composer-darwin-amd64"
      sha256 "..."
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/afintech/composer/releases/download/v0.11.0/composer-linux-arm64"
      sha256 "..."
    end
    on_intel do
      url "https://github.com/afintech/composer/releases/download/v0.11.0/composer-linux-amd64"
      sha256 "..."
    end
  end

  def install
    bin.install stable.url.split("/").last => "composer"
  end
end
```

**Automation**: Add a step to the release workflow that updates the formula
SHA256 hashes and pushes to the tap repo (or use `brew bump-formula-pr`-style
automation).

**Estimated scope**: New repo `afintech/homebrew-tap`, one formula file.

---

### Phase 4: Remove `COMPOSE_PROJECT_DIRECTORY` workaround

When composer runs on the host, its CWD *is* the project directory. The
`--project-directory` flag and `COMPOSE_PROJECT_DIRECTORY` env var exist
because the Docker container's CWD doesn't match the host.

**Changes to `src/compose.rs`:**

No code changes needed — the flag already works correctly, it just becomes
unnecessary. When running on the host, `composer -f ./compose.yml` in the
working directory already resolves env_file paths correctly because Docker
Compose inherits the real CWD.

The `--project-directory` flag stays for backward compat but is no longer
documented as required.

---

### Phase 5: Update brokerage2 deployment

**Goal**: Use host-native composer on brokerage2 EC2 instances.

#### 5a. Update `terraform/cme-databento/user_data.sh.tftpl`

Add to the bootstrap script:

```bash
# Install composer
COMPOSER_VERSION="v0.11.0"
ARCH=$(uname -m)
case "$ARCH" in
  x86_64) ARCH="amd64" ;;
  aarch64) ARCH="arm64" ;;
esac
curl -fsSL "https://github.com/afintech/composer/releases/download/${COMPOSER_VERSION}/composer-linux-${ARCH}" \
  -o /usr/local/bin/composer
chmod +x /usr/local/bin/composer

# Install bash aliases for interactive SSH sessions
sudo -u ec2-user composer install bash

# Install systemd unit (will start after configs are scp'd)
composer install systemd --user ec2-user --working-dir /home/ec2-user
```

#### 5b. Update `configs/prod/cme-databento/justfile`

Add to on-instance recipes:

```just
# Reload composer after config changes
reload-composer:
  sudo systemctl restart composer
```

Update the `deploy` recipe to restart composer if compose.yml changed (or
just rely on `WATCH_COMPOSE_FILE=true`).

#### 5c. Update `terraform/cme-databento/justfile` (laptop-side)

The `deploy` recipe stays the same — scp + ssh. Composer is already running
on the instance. If desired, add a `just restart-composer` recipe.

#### 5d. Same pattern for `cme-xignite` and future services.

#### 5e. Update `docs/internal/deployment.md`

Document the new bootstrap pattern: binary install, systemd unit, auto-start.

---

### Phase 6: Update ax deployments (separate effort)

For the ax repo, the migration is:

1. Remove the `composer` service from compose.yml files
   (`configs/grafana/compose.yml`, `docs/internal/compose.yml`, etc.)
2. Update EC2 user_data / `setup-ec2-local.sh` to install the binary and
   systemd unit instead of running composer as a container.
3. The `composer.zsh` / `composer.bash` shell aliases continue to work
   unchanged — they wrap `docker compose`, not the composer daemon.
4. The `status` function in the aliases still hits `localhost:10080` — this
   works because the host daemon binds the same port.

This is a separate PR / effort and doesn't block the brokerage2 work.

---

## What Does NOT Change

- **Label-based scheduling**: `co.architect.composer.run`, `.restart` labels
  on Docker Compose services. No config format changes.
- **Shell aliases**: `composer.zsh` and `composer.bash` are pure `docker
  compose` wrappers. They work identically regardless of how the daemon runs.
- **Status endpoint**: Still `http://localhost:10080/status.txt`.
- **Slack notifications, certificate monitoring, system monitoring**: All
  work the same (and system monitoring gets *simpler* — no privileged container).
- **CLI interface**: Same flags, same env vars, same subcommands.
- **Docker image**: Kept for backward compatibility. Not removed.

## What Gets Simpler

| Concern | Before (container) | After (host daemon) |
|---------|-------------------|-------------------|
| Docker socket | Volume mount required | Native access |
| Compose file | Volume mount at `/compose.yml` | Read from CWD |
| Env files | Dual-mount at `/` and `$HOME` | Read from CWD |
| Log directory | Volume mount | Write directly to host |
| System monitoring | `pid: host`, `privileged`, `network_mode: host` | Native (no special perms) |
| Project directory | `COMPOSE_PROJECT_DIRECTORY` env var | Just CWD |
| compose.yml | Extra `composer` service definition | No composer service |
| EC2 install | Pull Docker image, configure mounts | `curl` binary, `install systemd` |
| Upgrades | Pull new image, restart container | Download new binary, restart service |

## Migration Order

1. **Phase 1** (release pipeline) — unblocks everything else
2. **Phase 2** (install subcommands) — the key UX improvement
3. **Phase 3** (Homebrew) — nice-to-have, can be done in parallel
4. **Phase 5** (brokerage2) — first consumer of the new pattern
5. **Phase 6** (ax) — broader rollout, lower priority

Phases 1+2 are the critical path. A single PR to the composer repo.

## Open Questions

- **Versioning strategy**: Pin a specific version in user_data, or use `latest`?
  Recommend pinning for production (reproducible deploys) and `latest` for dev.
- **Auto-update**: Should composer support self-update (`composer update`)?
  Probably not in v1 — keep it simple, update via user_data or manual curl.
- **`composer install zsh`**: The ax repo uses `composer.zsh` (richer, with
  `--profile` support). Should the embedded aliases grow zsh support, or
  should the zsh aliases stay in the ax repo? Recommend: keep zsh aliases in
  ax for now, since they're tightly coupled to the ax workflow.
- **macOS launchd**: Worth building, or just document `composer &` in a
  terminal? Probably low priority since local dev doesn't usually need the
  scheduler daemon.
