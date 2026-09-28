# ricepilot acceptance sandbox (M5 task F).
#
# Built by packaging/acceptance/run.sh from a STAGED context under
# target/acceptance/ctx/: `git archive HEAD` of the repository plus this
# directory. Nothing else from the host goes in: no .git, no target/, no
# untracked file, no home directory. The network is used only here, at build
# time (pacman, cargo fetch); the container itself runs with --network none.
#
# The digest is pinned so a re-run builds on the same base; bump it on purpose.
ARG BASE=archlinux:latest
FROM ${BASE}

# rust/gcc to build ricepilot here (rolling Arch and the host's glibc can
# differ, so the host binary is never copied in); git for the fake caelestia
# clone; dash + busybox + bash to run rescue.sh under three shells; strace for
# crash injection at an exact syscall; python for the fake Wayland socket.
# No Hyprland, hyprctl or uwsm package: the stand-ins in fakes/ are the only
# ones, at /usr/local/bin, which is where ricepilot's allowlist looks after
# /usr/bin.
RUN pacman -Syu --noconfirm --needed rust gcc git dash busybox strace python diffutils \
 && pacman -Scc --noconfirm \
 && ! test -e /usr/bin/Hyprland && ! test -e /usr/bin/hyprctl && ! test -e /usr/bin/uwsm

# Two unprivileged users: rice (the one whose rice this is) and rice2 (the D74
# lock-owner check). Each has a runtime directory the way logind would make it.
RUN useradd -m -u 1000 -s /bin/bash rice \
 && useradd -m -u 1001 -s /bin/bash rice2 \
 && install -d -m 0700 -o rice -g rice /run/user/1000 \
 && install -d -m 0700 -o rice2 -g rice2 /run/user/1001 \
 && install -d -m 1777 /var/log/fakes \
 && install -d -m 1777 /srv/shared-runtime

COPY ricepilot.tar /opt/ricepilot.tar
RUN install -d -o rice -g rice /src \
 && su rice -c 'tar -xf /opt/ricepilot.tar -C /src' \
 && rm /opt/ricepilot.tar

WORKDIR /src
# Fetch as rice (network allowed at build time only), then build offline.
RUN su rice -c 'cargo fetch --locked' \
 && su rice -c 'cargo build --release --locked --frozen' \
 && install -m 0755 target/release/ricepilot /usr/local/bin/ricepilot

COPY fakes/Hyprland fakes/hyprctl fakes/uwsm /usr/local/bin/
COPY setup-rice.sh steps.sh lock-check.sh /acceptance/
RUN chmod 0755 /usr/local/bin/Hyprland /usr/local/bin/hyprctl /usr/local/bin/uwsm \
 && chmod 0755 /acceptance/*.sh

USER rice
WORKDIR /home/rice
CMD ["sleep", "infinity"]
