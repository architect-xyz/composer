# Default command: run both format and lint
default:
    @just --list 

# Format code using rustfmt
format:
    cargo +nightly fmt

# Check for unused dependencies using cargo-machete
lint:
    cargo machete

