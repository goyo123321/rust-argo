# 使用较新 Rust 镜像（包含 cargo 和 rustc）
FROM rust:1.85 AS builder

# 设置环境变量以增强网络稳定性
ENV CARGO_NET_RETRY=3 \
    CARGO_NET_TIMEOUT=30

WORKDIR /build

# 首先复制 Cargo.toml 和 Cargo.lock（如果有）
COPY Cargo.toml Cargo.lock* ./

# 创建虚拟主文件以提前构建依赖
RUN mkdir src && echo "fn main() {}" > src/main.rs

# 下载依赖并构建虚拟项目（利用 Docker 层缓存）
RUN cargo fetch --locked || cargo fetch  # 如果无 Cargo.lock 则去掉 --locked
RUN cargo build --release --locked || cargo build --release

# 现在复制完整源代码，之前的依赖层已缓存
COPY . .

# 重新构建（此时只会编译实际代码）
RUN cargo build --release --locked || cargo build --release

# 运行阶段
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
RUN useradd -m -u 1000 app
WORKDIR /app

# 复制编译好的二进制和静态文件
COPY --from=builder /build/target/release/rust-app /app/rust-app
COPY index.html /app/index.html

RUN mkdir -p /app/tmp && chown -R app:app /app

USER app

ENV FILE_PATH=/app/tmp \
    RUST_LOG=info

EXPOSE 3000 7860
CMD ["/app/rust-app"]
