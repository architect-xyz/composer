FROM rust:1.81-alpine AS builder
ADD . /composer
WORKDIR /composer
RUN cargo build --release

FROM alpine:3
COPY --from=builder /composer/target/release/composer /usr/local/bin/composer
CMD ["composer"]