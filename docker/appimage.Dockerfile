FROM almalinux:8

# Match the CI AppImage job's glibc 2.28 baseline and runtime-only display libraries.
RUN dnf install -y --setopt=install_weak_deps=False \
        gcc gcc-c++ make perl perl-IPC-Cmd pkgconf-pkg-config git curl tar gzip \
        ca-certificates file desktop-file-utils \
        libX11 libX11-xcb libxcb libXcursor libXi libxkbcommon libxkbcommon-x11 \
        libwayland-client libwayland-cursor libwayland-egl \
    && dnf clean all

RUN curl --proto '=https' --tlsv1.2 -fsSL https://sh.rustup.rs -o /tmp/rustup.sh \
    && sh /tmp/rustup.sh -y --profile minimal --default-toolchain stable \
    && rm /tmp/rustup.sh

ENV PATH="/root/.cargo/bin:${PATH}" \
    CARGO_TARGET_DIR=/target

# The mounted checkout belongs to the host. Keep Linux output on a Docker volume,
# away from native target/ files and Windows filesystem symlink restrictions.
RUN git config --global --add safe.directory /work
WORKDIR /work
CMD ["bash", "-c", "cargo build --release --locked && bash scripts/package-appimage.sh"]
