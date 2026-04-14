FROM rust:latest

WORKDIR /app

RUN cargo install cargo-watch

COPY . .

CMD ["cargo", "watch", "-x", "run"]
