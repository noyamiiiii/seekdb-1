# Base artifacts are pinned by content. Registry overrides are only for local
# mirror access; the tested artifacts use the same immutable digests.
ARG RUST_IMAGE=rust:1.88-slim-bookworm@sha256:38bc5a86d998772d4aec2348656ed21438d20fcdce2795b56ca434cf21430d89
ARG RUNTIME_IMAGE=debian:bookworm-slim@sha256:734a72b3d6142a71993b4bc580ca01d8ca687f77a2c422d96a961d3376b179bc
FROM ${RUST_IMAGE} AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY .cargo ./.cargo
COPY vendor ./vendor
COPY src ./src
COPY examples ./examples
COPY tests ./tests
RUN cargo test --release --locked --offline && cargo build --release --locked --offline --bin minivectordb
FROM ${RUNTIME_IMAGE}
RUN mkdir -p /data
COPY --from=build /src/target/release/minivectordb /usr/local/bin/minivectordb
VOLUME ["/data"]
WORKDIR /data
ENTRYPOINT ["/usr/local/bin/minivectordb"]
CMD ["--data-dir", "/data"]
