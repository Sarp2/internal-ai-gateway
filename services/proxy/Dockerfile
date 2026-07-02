FROM rust:1-alpine AS build

WORKDIR /app

RUN apk add --no-cache musl-dev

COPY services/proxy/Cargo.toml services/proxy/Cargo.lock ./services/proxy/
COPY services/proxy/src ./services/proxy/src

RUN cargo build --manifest-path services/proxy/Cargo.toml --release

FROM alpine:3.22

ENV PORT=8080
ENV RUST_LOG=info

WORKDIR /app

COPY --from=build /app/target/release/proxy-service ./proxy-service

EXPOSE 8080

CMD ["./proxy-service"]
