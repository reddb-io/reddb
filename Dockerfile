FROM rust:1 AS builder

WORKDIR /app

COPY . .

RUN cargo build --release --bins

FROM debian:bookworm-slim

RUN set -eux; \
    apt-get update; \
    apt-get install -y --no-install-recommends ca-certificates; \
    rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder /app/target/release/reddb /usr/local/bin/reddb
COPY --from=builder /app/target/release/reddb-grpc /usr/local/bin/reddb-grpc

RUN chmod +x /usr/local/bin/reddb /usr/local/bin/reddb-grpc

VOLUME /data

EXPOSE 8080 50051

ENV REDDB_PATH=/data/reddb.rdb

ENTRYPOINT ["/usr/local/bin/reddb"]
CMD ["--path", "/data/reddb.rdb", "--bind", "0.0.0.0:8080"]

