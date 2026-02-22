# 构建阶段：编译 Rust 程序
FROM rust:1.70 AS builder
WORKDIR /build

COPY . .
RUN cargo build --release

# 运行阶段
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
RUN useradd -m -u 1000 app

WORKDIR /app

# 复制二进制和静态文件
COPY --from=builder /build/target/release/rust-app /app/rust-app
COPY index.html /app/index.html

RUN mkdir -p /app/tmp && chown -R app:app /app

USER app

ENV FILE_PATH=/app/tmp
ENV RUST_LOG=info

EXPOSE 3000 7860
CMD ["/app/rust-app"]