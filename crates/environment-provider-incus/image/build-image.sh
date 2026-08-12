#!/bin/sh
set -eu

# Run on the Incus image builder after copying release lightspeed-envd to the
# current directory. The resulting alias is only a convenience: production
# provider configuration must use the immutable fingerprint printed by Incus.
incus launch images:ubuntu/24.04/cloud lightspeed-dev-image --vm
incus exec lightspeed-dev-image -- sh -euxc 'apt-get update && apt-get install -y git docker.io docker-compose-v2 build-essential pkg-config curl ca-certificates && useradd --system --create-home --home-dir /var/lib/lightspeed-envd lightspeed-envd && mkdir -p /workspace /etc/lightspeed-envd && chown lightspeed-envd:lightspeed-envd /workspace'
incus file push ./lightspeed-envd lightspeed-dev-image/usr/local/bin/lightspeed-envd --mode 0755
incus file push ./lightspeed-envd.service lightspeed-dev-image/etc/systemd/system/lightspeed-envd.service --mode 0644
incus stop lightspeed-dev-image
incus publish lightspeed-dev-image --alias lightspeed-dev-v1
incus delete lightspeed-dev-image
incus image info lightspeed-dev-v1
