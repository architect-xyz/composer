FROM rust:1.88-alpine AS builder
RUN apk add --no-cache musl-dev libressl-dev
ADD . /composer
WORKDIR /composer
RUN cargo build --release

FROM docker:27-cli 
COPY --from=builder /composer/target/release/composer /usr/local/bin/composer
ENTRYPOINT ["composer"]
CMD ["--env-file", "/.env"]