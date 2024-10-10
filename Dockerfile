FROM rust:1.81-alpine AS builder
RUN apk add --no-cache musl-dev
ADD . /composer
WORKDIR /composer
RUN cargo build --release

FROM docker:27-cli 
COPY --from=builder /composer/target/release/composer /usr/local/bin/composer
ENV COMPOSE_PROJECT_NAME="composer"
CMD ["composer", "-f", "/compose.yml"]