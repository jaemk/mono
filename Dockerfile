FROM rust:1.65.0-bullseye as builder

# create a new empty shell
RUN mkdir -p /app
WORKDIR /app

RUN USER=root cargo new --bin mono
WORKDIR /app/mono

# copy over your manifests
COPY ./Cargo.toml ./Cargo.toml
COPY ./Cargo.lock ./Cargo.lock

# this build step will cache your dependencies
RUN cargo build --release
RUN rm src/*.rs

# copy all source/static/resource files
COPY ./src ./src
COPY ./static ./static
# COPY ./templates ./templates

# build for release
RUN rm ./target/release/deps/mono*
RUN cargo build --release

# copy over git dir and embed latest commit hash
COPY ./.git ./.git
# make sure there's no trailing newline
RUN git rev-parse HEAD | awk '{ printf "%s", substr($0, 0, 7)>"commit_hash.txt" }'
RUN rm -rf ./.git

# copy out the binary, static assets, and commit_hash
FROM debian:bullseye-slim
WORKDIR /app/mono
COPY --from=builder /app/mono/commit_hash.txt ./commit_hash.txt
COPY --from=builder /app/mono/static ./static
COPY --from=builder /app/mono/target/release/mono ./mono

CMD ["./mono"]
