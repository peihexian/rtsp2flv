# Build stage
FROM ubuntu:24.04 as builder

ENV DEBIAN_FRONTEND=noninteractive

# Install build dependencies and FFmpeg 7
RUN apt-get update && apt-get install -y \
    curl \
    build-essential \
    pkg-config \
    libssl-dev \
    software-properties-common \
    git \
    clang \
    && add-apt-repository ppa:ubuntuhandbook1/ffmpeg7 -y \
    && apt-get update \
    && apt-get install -y \
    ffmpeg \
    libavcodec-dev \
    libavdevice-dev \
    libavfilter-dev \
    libavformat-dev \
    libavutil-dev \
    libswresample-dev \
    libswscale-dev \
    && rm -rf /var/lib/apt/lists/*

# Install Rust
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
ENV PATH="/root/.cargo/bin:${PATH}"

WORKDIR /app
COPY . .

# Build release
RUN cargo build --release

# Runtime stage
FROM ubuntu:24.04

ENV DEBIAN_FRONTEND=noninteractive

# Install runtime dependencies (FFmpeg 7)
RUN apt-get update && apt-get install -y \
    software-properties-common \
    ca-certificates \
    openssl \
    && add-apt-repository ppa:ubuntuhandbook1/ffmpeg7 -y \
    && apt-get update \
    && apt-get install -y \
    ffmpeg \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY --from=builder /app/target/release/rtsp2flv /app/rtsp2flv
COPY config.yaml /app/config.yaml
COPY web /app/web

# Expose port (default 3000 based on common axum apps, but user config might vary)
EXPOSE 3000

CMD ["./rtsp2flv"]
