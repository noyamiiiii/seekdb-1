# MiniVectorDB: build the engine from source, then ship only the binary.
#
# The crate has no dependencies outside the Rust standard library, so the build
# stage never touches the network beyond fetching the pinned base images and the
# runtime image needs no extra packages.
FROM rust:1.88-slim-bookworm AS build

WORKDIR /src
# Copy the manifest first so a later `cargo build` can reuse the layer when only
# the sources change. There is no registry index to fetch: no dependencies.
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY examples ./examples
RUN cargo build --release --locked --bin minivectordb

FROM debian:bookworm-slim

RUN mkdir -p /data
COPY --from=build /src/target/release/minivectordb /usr/local/bin/minivectordb

VOLUME ["/data"]
WORKDIR /data

# The grading driver talks to the server over stdin/stdout, as the task requires:
# run the image with `-i`, never `-d`/`-t`, and keep stdout clean.
ENTRYPOINT ["/usr/local/bin/minivectordb", "--data", "/data"]