FROM mcr.microsoft.com/devcontainers/rust:1-bookworm

# Install LLVM 22 from the official apt.llvm.org repository
RUN apt-get update && apt-get install -y --no-install-recommends \
    wget \
    gnupg \
    lsb-release \
    software-properties-common \
    && wget -qO- https://apt.llvm.org/llvm-snapshot.gpg.key | tee /etc/apt/trusted.gpg.d/apt.llvm.org.asc \
    && echo "deb http://apt.llvm.org/bookworm/ llvm-toolchain-bookworm-22 main" > /etc/apt/sources.list.d/llvm-22.list \
    && apt-get update && apt-get install -y --no-install-recommends \
    llvm-22 \
    llvm-22-dev \
    llvm-22-runtime \
    llvm-22-tools \
    llvm-22-linker-tools \
    clang-22 \
    lld-22 \
    lldb-22 \
    pkg-config \
    && rm -rf /var/lib/apt/lists/* \
    && update-alternatives --install /usr/bin/llvm-config llvm-config /usr/bin/llvm-config-22 100 \
    && update-alternatives --install /usr/bin/clang clang /usr/bin/clang-22 100 \
    && update-alternatives --install /usr/bin/lld lld /usr/bin/lld-22 100 \
    && update-alternatives --install /usr/bin/lldb lldb /usr/bin/lldb-22 100

RUN rustup component add clippy rustfmt rust-src

ENV LLVM_SYS_221_PREFIX=/usr/lib/llvm-22
