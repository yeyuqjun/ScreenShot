FROM rust:1-slim-bookworm AS build
WORKDIR /build
COPY Cargo.toml Cargo.lock schema.sql ./
COPY src ./src
RUN cargo build --locked --release --bin snapdock-server

FROM debian:bookworm-slim
RUN useradd --system --uid 10001 --create-home snapdock && mkdir /data && chown snapdock:snapdock /data
COPY --from=build /build/target/release/snapdock-server /usr/local/bin/snapdock-server
USER snapdock
WORKDIR /data
EXPOSE 8899
VOLUME ["/data"]
ENTRYPOINT ["snapdock-server", "--data-dir", "/data"]
CMD ["serve", "--bind", "0.0.0.0:8899"]
