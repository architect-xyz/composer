# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Composer is a Docker Compose scheduler written in Rust that runs or restarts services on cron schedules. It leverages the Docker CLI to parse compose configurations and supports:

- Scheduled service runs and restarts using cron expressions
- Slack notifications for job status
- Log capture to files
- Docker image pruning on schedule
- Support for compose profiles and environment files

## Architecture

### Core Components

- **src/main.rs**: CLI entry point and main scheduler loop using tokio JoinSet
- **src/compose.rs**: Docker compose command builder and configuration loading
- **src/compose_types.rs**: Serde types for parsing docker-compose.yml files
- **src/scheduler.rs**: Generic command scheduling functionality

### Key Concepts

- Services are scheduled using labels: `co.architect.composer.run` or `co.architect.composer.restart`
- Cron expressions are Quartz-compatible (6 fields with seconds first)
- The scheduler runs as a Docker service itself, mounting the Docker socket
- Logs can be captured to files or output to console for debugging

## Development Commands

### Building and Running
```bash
# Build the project
cargo build

# Run with debug logging
RUST_LOG=composer=debug cargo run -- -f compose.yml

# Format code (uses custom rustfmt.toml)
cargo fmt

# Run clippy linting
cargo clippy

# Run tests
cargo test
```

### Docker Development
```bash
# Build Docker image
docker build -t composer .

# Run the example scheduler
docker compose up scheduler

# View logs from scheduled jobs
docker compose logs -f
```

### Testing
```bash
# Run the test script (uses ./test.sh and compose.yml example)
./test.sh

# Test with custom compose file
cargo run -- -f /path/to/compose.yml --run-logs ./logs
```

## Configuration

### Environment Variables
- `RUST_LOG`: Set log level (e.g., `composer=debug`)
- `COMPOSE_PROJECT_NAME`: Override compose project name
- `COMPOSE_PROJECT_DIRECTORY`: Set working directory for compose commands
- `COMPOSE_RUN_LOGS`: Directory for job output logs
- `HOST`: Hostname for Slack notifications
- `SLACK_WEBHOOK_URL`: Slack webhook for all notifications
- `SLACK_WEBHOOK_ON_ERROR_URL`: Slack webhook for error-only notifications
- `PRUNE_IMAGES`: Cron schedule for `docker image prune -f`

### Service Labels
- `co.architect.composer.run`: Cron expression for running service
- `co.architect.composer.restart`: Cron expression for restarting service
- `co.architect.composer.notify.slack`: Enable Slack notifications (true/1)
- `co.architect.composer.notify.slack.on-error`: Enable error-only Slack notifications

## Dependencies

Key Rust crates:
- **tokio**: Async runtime with process spawning
- **clap**: CLI argument parsing
- **cron**: Cron expression parsing and scheduling
- **serde/serde_yaml**: Docker compose file parsing
- **chrono**: DateTime handling for scheduling
- **reqwest**: HTTP client for Slack webhooks
- **anyhow**: Error handling

## Common Patterns

When modifying the scheduler:
- Use `tokio::process::Command` for spawning Docker commands
- Jobs run in separate async tasks via `JoinSet`
- Log output using the `log` crate with appropriate levels
- Always import log macros like `log::error`, and use them in code without qualification like `error!(...)`
- Always use `anyhow!` and `bail!` without qualification, importing them at the top of the file
- When printing errors, always use the debug output e.g. `{e:?}` for the full backtrace
- Handle both file-based and console logging paths
- Parse cron expressions using the `cron` crate's `Schedule` type