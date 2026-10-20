FROM debian:bookworm-slim

ARG TARGETARCH
COPY binaries/${TARGETARCH}/mesh-talk-node /usr/local/bin/mesh-talk-node

RUN groupadd --gid 10001 mesh-talk && \
    useradd --uid 10001 --gid 10001 --no-create-home --home-dir /var/lib/mesh-talk mesh-talk && \
    mkdir -p /var/lib/mesh-talk && \
    chown 10001:10001 /var/lib/mesh-talk

USER 10001:10001
WORKDIR /var/lib/mesh-talk
EXPOSE 47479/tcp 47474/udp 47478/udp
ENTRYPOINT ["/usr/local/bin/mesh-talk-node", "--post-office"]
