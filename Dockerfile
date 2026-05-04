FROM rust:1.95.0-bookworm as builder

RUN apt-get update && apt-get install --yes ca-certificates curl pkg-config libssl-dev libpq-dev
RUN cargo install migrant --features postgres

WORKDIR /app

# Copy manifests, lock file, and the stubbing script to cache dependency compilation
COPY bin/stub_workspace.sh bin/stub_workspace.sh
# Copy each crate's Cargo.toml – add a line here when a new crate is introduced
COPY ./Cargo.toml ./Cargo.toml
COPY crates/common/Cargo.toml crates/common/Cargo.toml
COPY crates/spot/Cargo.toml crates/spot/Cargo.toml
COPY crates/mono/Cargo.toml crates/mono/Cargo.toml
COPY crates/paste/Cargo.toml crates/paste/Cargo.toml

# Generate stub source files for every workspace member so cargo can
# compile and cache all third-party dependencies without the real source.
RUN chmod +x bin/stub_workspace.sh && bin/stub_workspace.sh .

RUN cargo build --release --bin mono

# Remove stub artifacts so the real source build is not skipped
RUN rm -f target/release/deps/mono* target/release/mono \
          target/release/deps/libcommon* target/release/deps/libspot* \
          target/release/deps/libpaste*

# Now copy the real source code and build
COPY . .
RUN cargo build --release --bin mono

# make sure there's no trailing newline for commit_hash
RUN git rev-parse HEAD | awk '{ printf "%s", substr($0, 0, 7) }' > commit_hash.txt

# Final stage
FROM debian:bookworm-slim
RUN apt-get update && apt-get install --yes ca-certificates curl libssl3 libpq5 && rm -rf /var/lib/apt/lists/*

COPY --from=builder /usr/local/cargo/bin/migrant /usr/bin/migrant

WORKDIR /app
COPY --from=builder /app/target/release/mono ./mono
COPY --from=builder /app/static ./static
COPY --from=builder /app/templates ./templates
COPY --from=builder /app/crates/paste/assets ./crates/paste/assets
COPY --from=builder /app/crates/paste/templates ./crates/paste/templates
COPY --from=builder /app/commit_hash.txt ./commit_hash.txt
COPY --from=builder /app/migrations ./migrations
COPY --from=builder /app/bin ./bin

CMD ["./bin/start.sh", "./mono"]
