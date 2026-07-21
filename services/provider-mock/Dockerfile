FROM rust:1-alpine AS build

WORKDIR /app

RUN apk add --no-cache build-base

COPY services/provider-mock/Cargo.toml services/provider-mock/Cargo.lock ./services/provider-mock/
COPY services/provider-mock/src ./services/provider-mock/src

RUN cargo build --manifest-path services/provider-mock/Cargo.toml --release

FROM alpine:3.22

ENV PORT=8080

WORKDIR /app

RUN addgroup -S provider-mock && adduser -S provider-mock -G provider-mock

COPY --from=build /app/target/release/provider-mock-service ./provider-mock-service

USER provider-mock

EXPOSE 8080

CMD ["./provider-mock-service"]
