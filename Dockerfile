FROM docker.io/library/rust:latest

WORKDIR /workspace

RUN cargo install cargo-nextest --locked

COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY tests ./tests
RUN cargo fetch --locked

CMD ["cargo", "nextest", "run", "--locked"]
