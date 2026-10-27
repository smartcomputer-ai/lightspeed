# Incus VMs

The included Incus provider creates and manages virtual machines for Lightspeed
environments. An operator supplies trusted images and templates; a universe
receives access through a provider binding. Universe Operators and Admins
create machines from those templates, then attach them to sessions or profiles.

The provider runs as a separate process. It talks to Incus using an HTTPS
client certificate, exposes a private controller to Lightspeed, and relays
environment operations to the daemon inside each VM. It does not connect to
Lightspeed's database or run session workflows.

This walkthrough uses one standalone Incus server. It builds an image,
registers the provider, binds a universe, and provisions a test environment.

## Prepare the host and trust boundary

Start with a Linux Incus installation that can run VMs, an initialized storage
pool, and working guest networking. The Incus CLI used to build the image
should target the same server the provider will use for this first setup.
Verify that the host has enough CPU, memory, and disk for the image-builder
VM and the environments you intend to run.

Prepare these files for the provider process:

- An Incus client certificate and private key authorized for the provider's
  project, network, image, profile, and VM operations.
- A trusted server CA file for validating the Incus HTTPS endpoint.
- A provider credential: a random secret of at least 32 bytes without
  whitespace, which Lightspeed presents to the provider.
- A provider configuration file with the endpoint, storage pool, and templates.

The provider must be able to reach Incus and the guest networks. Its
controller and data routes require the provider credential as a bearer token
and refuse other callers before the WebSocket upgrade. The guest daemon
listener inside each VM has no authentication of its own: keep guest networks
private, and do not expose the provider or its guests as public agent or
browser endpoints. [Networking and ingress](networking-and-ingress.md)
describes the separate authenticated runtime route and optional public app edge.

A Platform administrator registers the physical provider and binds universes.
Universe Operators and Admins can then create environments from the enabled
offerings.

## Build the provider and guest image

Use the same Lightspeed source revision as the runtime. On Linux, build for
the architecture of the provider host and intended guest:

```bash
cargo build --locked --release \
  -p environment-daemon \
  -p environment-provider-incus
```

The resulting binaries are `target/release/lightspeed-envd` and
`target/release/lightspeed-provider-incus`. A binary built for macOS cannot
be installed into the Linux image.

The repository's image builder expects the daemon binary and its service file
in its current working directory. From the checkout root, prepare that
directory and run the builder:

```bash
LIGHTSPEED_CHECKOUT="$(pwd)"
LIGHTSPEED_IMAGE_WORKDIR="$(mktemp -d)"
cp target/release/lightspeed-envd "$LIGHTSPEED_IMAGE_WORKDIR/"
cp crates/environment-provider-incus/image/lightspeed-envd.service \
  "$LIGHTSPEED_IMAGE_WORKDIR/"
(
  cd "$LIGHTSPEED_IMAGE_WORKDIR"
  sh "$LIGHTSPEED_CHECKOUT/crates/environment-provider-incus/image/build-image.sh"
)
```

The script uses the temporary instance name `lightspeed-dev-image` and the
image alias `lightspeed-dev-v1`; keep those names available for this build.
It creates an Ubuntu 24.04 cloud VM, installs development tools and the daemon,
stops and publishes the VM, then removes the builder instance. If the script
fails, inspect any remaining builder instance before retrying.

Record the immutable image fingerprint printed by the script. You can inspect
the published image again with:

```bash
incus image info lightspeed-dev-v1
```

The alias is a convenience for the build. The provider template must use the
fingerprint so a changed alias cannot silently change what a template creates.
If you use a separate image builder, make that exact image available to the
target server or configure the provider's trusted `imageServerUrl`.

The stock image installs Git, Docker packages, build tools, curl, and
certificates. Its service runs as the unprivileged `lightspeed-envd` user with
working directory `/workspace`. The systemd unit permits writes under
`/workspace` and `/var/lib/lightspeed-envd`. Installing Docker packages does
not by itself authorize that service user to administer Docker. Customize the
image and service for workloads that need different software or permissions.

## Configure and start the provider

Generate the provider credential and keep it readable only by the provider's
service account:

```bash
umask 077
openssl rand -hex 32 > /run/secrets/lightspeed-provider-credential
```

Create a configuration file such as `/etc/lightspeed/incus-provider.json`.
This example leaves public application ingress disabled:

```json
{
  "controllerListen": "127.0.0.1:19090",
  "credentialFile": "/run/secrets/lightspeed-provider-credential",
  "dataRouteBase": "ws://127.0.0.1:19090/routes",
  "incus": {
    "mode": "single",
    "endpoints": ["https://incus.internal:8443"],
    "clientCertificatePem": "/run/secrets/incus-client.crt",
    "clientPrivateKeyPem": "/run/secrets/incus-client.key",
    "serverCaPem": "/run/secrets/incus-server-ca.crt",
    "storagePool": "default"
  },
  "envdPort": 19091,
  "network": {
    "deniedEgressCidrs": ["169.254.169.254/32"]
  },
  "templates": [
    {
      "templateId": "dev-small-v1",
      "displayName": "Small development VM",
      "description": "2 vCPU, 4 GiB RAM, 40 GiB disk",
      "imageFingerprint": "<immutable image fingerprint>",
      "cpu": 2,
      "memory": "4GiB",
      "disk": "40GiB"
    }
  ]
}
```

Replace the Incus address, certificate paths, pool, and fingerprint with your
installation's values. Template `memory` and `disk` are whole Incus sizes such
as `4GiB`; the provider reports them to Lightspeed in bytes. The sample
controller listens on loopback, which works when the environment gateway can
reach that same host namespace. For separate hosts or containers, bind an
appropriate private address and register that reachable address instead.

`dataRouteBase` is the URL at which Lightspeed reaches this listener's
`/routes` path, so it must use the address Lightspeed dials, not necessarily
the bind address. The provider advertises it during the controller handshake.
When Lightspeed reaches the controller through TLS, for example behind a
reverse proxy, `dataRouteBase` must use `wss://` as well.

The network block denies the metadata-service address in this example. It
is an explicit egress rule, not a complete network policy. The provider also
separates sibling binding networks. Choose additional denied destinations for
your deployment, avoiding overlap with allocated guest network ranges.

Start the process:

```bash
target/release/lightspeed-provider-incus \
  --config /etc/lightspeed/incus-provider.json
```

Keep the process running under your service manager for a lasting installation,
with access to its configuration and certificate files. In another terminal
on the same host, check:

```bash
curl --fail http://127.0.0.1:19090/health
```

The health response checks Incus topology and needs no credential. It does not
prove that the image can boot or that a guest daemon is reachable; the first
environment test covers those steps.

The complete [example configuration](../../../crates/environment-provider-incus/config.example.json)
also includes optional relay settings, an image server, and ingress.
`relayIdleSeconds` cannot be set below 60, so a quiet long read is never cut
by the relay. The provider config has no provider ID or list of authorized
universes. Those belong to Lightspeed's registry and bindings.

## Register the provider and bind a universe

In the Platform administrator area, open **Environment providers → Register
provider**. Enter:

| Field | Value for the same-host example |
| --- | --- |
| Provider id | `incus-local` |
| Display name (optional) | `Local Incus` |
| Controller endpoint | `ws://127.0.0.1:19090/control` |
| Credential | The contents of the provider credential file |

Choose **Register**. Lightspeed connects to the controller and checks its
handshake. A wrong path or credential refuses the registration; a provider
that is not running yet is accepted and checked again later. The credential
is write-only: Lightspeed never shows it again, and leaving it empty when
editing the provider keeps the stored one. The ID is a stable
deployment-wide identifier used by its universe bindings. For another
topology, use the private endpoint reachable from the environment gateway
rather than its own loopback address.

Choose **Bind universe**, select the **Universe**, leave **Binding id** at the
provider-ID default or enter a stable ID, and choose **Bind**. There can be at
most one binding for each universe/provider pair.

An enabled binding lets that universe list the provider's templates and create
environments. The provider lazily creates a restricted Incus project, managed
network, ACL, and profile for the binding. You do not need to add the universe
to the provider JSON or restart it to add another binding.

## Provision and verify a VM

In the bound universe, open **Environments → New environment**. Choose
**Small development VM**, enter a display name such as `Acorn build machine`,
and choose **Provision**. You can leave the idle policy empty for this initial
test, then configure it under [Power and cleanup](power-and-cleanup.md).

The environment progresses through provisioning and booting toward ready.
Expand **Details** to inspect the environment ID, provider, template, and
provider target. Attach it with **Run commands** access and select it in a session
with **Environments** enabled, then ask the agent to run `pwd` and a harmless
file check.

The provider configures the guest daemon during provisioning. The stock setup
listens privately on port 19091, starts commands in `/workspace`, and keeps
its state in `/var/lib/lightspeed-envd`, outside the agent's working tree. It
uses passive connectivity rather than a daemon registration key. The provider
relays requests to it through the guest network.

Continue with [Processes and jobs](processes-and-jobs.md) for a complete file
and execution check. Finish the test by closing the disposable environment
and waiting for its status to become closed. Closing releases the VM and its
disk, so save any needed output first.

## Update offerings and manage access

Publish a changed image under a new fingerprint and a new template ID. Set
`deprecated: true` on the old template when it should stop appearing for new
environments. Deprecation affects only new environments: existing ones keep
waking, powering, and closing normally. Edit the provider's `templates`
configuration and restart its service: configuration is loaded at startup,
with no hot reload.

Retain old template entries while their environments still need them, because
operations such as ingress enablement consult that configuration. Existing
machines retain their original setup; changing an image does not upgrade their
files or running processes. A VM whose image carries a daemon that speaks
another environment protocol version than the runtime fails when it becomes
ready, with an error naming both versions; recreate it from a current
template.

Disabling a universe binding prevents new provisioning. It does not close
environments already created through it. Binding deletion is refused while
nonclosed environments reference it, and provider deletion is refused while
bindings reference it. Removing registry records does not automatically remove
all provider-side projects or network infrastructure.

## Use an existing Incus cluster

Cluster mode connects to an already formed native Incus cluster. Configure
`mode: "cluster"`, HTTPS endpoints belonging to that same cluster, the storage
pool on eligible members, and the existing `clusterNetworkUplink` for managed
OVN networks. The provider and optional application edge need network access
to the guests on those networks.

An optional template `clusterGroup` restricts placement to that group. Incus
chooses the member; Lightspeed does not implement a second scheduler. Multiple
API endpoints provide another route to the cluster API, not automatic recovery
of a VM whose storage or member is unavailable.

Cluster formation, member maintenance, evacuation, and recovery remain operator
tasks. Follow the [cluster operations guide](../../../crates/environment-provider-incus/CLUSTER.md)
and [cluster configuration example](../../../crates/environment-provider-incus/config.cluster.example.json)
before adopting that topology.

## If provisioning fails

| Symptom | What to check |
| --- | --- |
| The provider cannot start or its health check fails | Check configuration, Incus HTTPS trust, client permissions, topology mode, and endpoint reachability. |
| The universe has no templates | Check provider registration, the universe's enabled binding, and template deprecation. |
| Incus cannot find the configured image | Use the immutable fingerprint, make it available on the target, or configure a trusted image server. |
| Registering the provider is refused | Check the controller URL path, the credential, and that `dataRouteBase` uses `wss://` when the controller is reached over TLS. |
| A VM boots but its environment is not ready | Check the guest daemon service, matching binary architecture, provider-to-guest routes, private daemon port, and that `dataRouteBase` reaches the provider from the environment gateway. |
| The environment fails naming two protocol versions | The image's daemon does not match the runtime release. Rebuild the image and recreate the environment. |
| A command cannot write outside the workspace | Inspect the stock service's systemd restrictions and daemon-user permissions. Customize the image deliberately if the task needs more access. |
| Adding a binding fails during network setup | Check Incus permissions, subnet availability, denied-CIDR overlap, and cluster uplink configuration if applicable. |
