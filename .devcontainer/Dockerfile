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
    libclang-rt-22-dev \
    libpolly-22-dev \
    lld-22 \
    lldb-22 \
    pkg-config \
    # LLVM 22's static libs reference zstd; linking lin needs the libzstd.so dev
    # symlink (the runtime libzstd1 alone gives only .so.1 → "-lzstd not found").
    libzstd-dev \
    && rm -rf /var/lib/apt/lists/* \
    && update-alternatives --install /usr/bin/llvm-config llvm-config /usr/bin/llvm-config-22 100 \
    && update-alternatives --install /usr/bin/clang clang /usr/bin/clang-22 100 \
    && update-alternatives --install /usr/bin/lld lld /usr/bin/lld-22 100 \
    && update-alternatives --install /usr/bin/lldb lldb /usr/bin/lldb-22 100

RUN rustup component add clippy rustfmt rust-src

# Debugging toolchain for the runtime (UAF / heisenbug / logic-bug hunting):
#   - gdb       : watchpoints on refcount words, single-stepping the runtime
#   - valgrind  : memcheck catches UNINITIALISED reads that ASan does not (MSan-class bugs)
#   - rr        : deterministic record/replay + reverse debugging (needs kernel.perf_event_paranoid<=1;
#                 not usable under the default container perf restriction, installed for hosts that allow it)
RUN apt-get update && apt-get install -y --no-install-recommends \
    gdb \
    valgrind \
    rr \
    # GNU time (/usr/bin/time -v): peak RSS + wall-clock for the RAPTOR/perf benches
    # (the shell builtin `time` reports neither; the benches' memory budget is tracked via this)
    time \
    # perf: sampling profiler for the RELEASE benches. NOTE: this container runs with
    # kernel.perf_event_paranoid=4 and no writable sysctl, so perf only works as root and
    # with a SOFTWARE event (e.g. `sudo perf record -e cpu-clock -g -- ./bin`); hardware PMU
    # counters are unavailable. AVX-512 binaries are fine (perf samples, it doesn't decode).
    linux-perf \
    && rm -rf /var/lib/apt/lists/*

ENV LLVM_SYS_221_PREFIX=/usr/lib/llvm-22
