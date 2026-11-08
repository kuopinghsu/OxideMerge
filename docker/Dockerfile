# Test environment for checking OxideMerge on Linux from any machine with Docker:
# scripts/docker-test.sh builds and runs it; see docs/development.md. It holds Rust, the
# libraries the program loads, Xvfb with Mesa, Git, Subversion and Mercurial, and the servers
# docker/servers.sh starts: OpenSSH, vsftpd, ProFTPD, Dante and tinyproxy.

ARG RUST_VERSION=1.99.1

FROM rust:${RUST_VERSION}-slim-bookworm
# Installed services are not started: the tests start their own.
RUN printf '#!/bin/sh\nexit 101\n' >/usr/sbin/policy-rc.d && chmod +x /usr/sbin/policy-rc.d \
    && apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        build-essential pkg-config perl libxkbcommon-dev libgl1-mesa-dev \
        xvfb xauth libgl1-mesa-dri libegl-mesa0 libxkbcommon-x11-0 \
        libx11-xcb1 libxcursor1 libxi6 git subversion mercurial locales \
        fonts-noto-cjk fonts-noto-color-emoji python3 openssl \
        openssh-server openssh-client netcat-openbsd proftpd-core proftpd-mod-crypto \
        libwrap0 dante-server tinyproxy \
    # vsftpd and ProFTPD conflict as packages, but run side by side.
    && cd /tmp && apt-get download vsftpd && dpkg-deb -x vsftpd_*.deb vsftpd \
    && cp vsftpd/usr/sbin/vsftpd /usr/sbin/ && cp vsftpd/etc/pam.d/vsftpd /etc/pam.d/ \
    && rm -r vsftpd vsftpd_*.deb \
    && rm -rf /var/lib/apt/lists/* \
    && localedef -i en_US -f UTF-8 en_US.UTF-8 \
    && rustup component add clippy \
    # A container has no login session to record, which would fail every SSH sign-in.
    && sed -i 's/^session\s\+required\s\+pam_loginuid.so/session optional pam_loginuid.so/' /etc/pam.d/sshd \
    && mkdir -p /run/sshd \
    # The tests run as an ordinary user: as root, unreadable files would read fine.
    && useradd -m -u 1000 -s /bin/sh tester \
    && mkdir -p /usr/local/cargo/registry /build /work \
    && chown -R tester:tester /usr/local/cargo /build /work
ENV LANG=en_US.UTF-8 LC_ALL=en_US.UTF-8 CARGO_TARGET_DIR=/build \
    LIBGL_ALWAYS_SOFTWARE=1 GUI_SMOKE_STRICT=1 CARGO_TERM_COLOR=never
WORKDIR /work
