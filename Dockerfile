FROM rust:1.58.1-bullseye as builder

# create a new empty shell
RUN mkdir -p /app
WORKDIR /app

RUN USER=root cargo new --bin ugh
WORKDIR /app/ugh

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
RUN rm ./target/release/deps/ugh*
RUN cargo build --release

# copy over git dir and embed latest commit hash
COPY ./.git ./.git
# make sure there's no trailing newline
RUN git rev-parse HEAD | awk '{ printf "%s", substr($0, 0, 7)>"commit_hash.txt" }'
RUN rm -rf ./.git

# copy out the binary, static assets, and commit_hash
FROM debian:bullseye-slim
WORKDIR /app/ugh
COPY --from=builder /app/ugh/commit_hash.txt ./commit_hash.txt
COPY --from=builder /app/ugh/static ./static
COPY --from=builder /app/ugh/target/release/ugh ./ugh

CMD ["./ugh"]
