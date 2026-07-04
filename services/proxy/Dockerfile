FROM rust:1-alpine AS build

WORKDIR /app

RUN apk add --no-cache build-base ca-certificates cmake perl

COPY services/proxy/Cargo.toml services/proxy/Cargo.lock ./services/proxy/
COPY services/proxy/src ./services/proxy/src

RUN cargo build --manifest-path services/proxy/Cargo.toml --release

FROM alpine:3.22

ENV PORT=8080
ENV RUST_LOG=info

WORKDIR /app

RUN apk add --no-cache ca-certificates

COPY --from=build /app/target/release/proxy-service ./proxy-service

EXPOSE 8080

CMD ["./proxy-service"]
