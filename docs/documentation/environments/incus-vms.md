# Incus VMs

The included Incus provider creates and manages virtual machines for Lightspeed
environments. A deployment operator supplies trusted images and templates.
Universe Operators and Owners use those templates to create machines for
sessions and profiles.

The provider runs as a separate process. It talks to Incus using an HTTPS
client certificate, exposes a private controller to Lightspeed, and relays
environment operations to the daemon inside each VM. It can pause or stop idle
VMs and wake them when Lightspeed connects.

This walkthrough builds an image on a standalone Incus server, registers the
provider, and provisions a test environment.

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
  whitespace, which Lightspeed presents to the provider. The credential is
  optional on networks where only Lightspeed can reach the listener.
- A provider configuration file with the endpoint, storage pool, and templates.

The provider must be able to reach Incus and the guest networks. With a
credential configured, its controller and data routes require it as a bearer
token and refuse other callers before the WebSocket upgrade. The guest daemon
listener inside each VM has no authentication of its own: keep guest networks
private, and do not expose the provider or its guests as public agent or
browser endpoints. [Networking and ingress](networking-and-ingress.md)
describes the separate authenticated runtime route and optional public app edge.

A Platform Admin registers the provider and chooses which universes can use
it. Operators and Owners in those universes can provision from its templates.

## Build the provider and guest image

Use the same Lightspeed source revision as the runtime. On Linux, build for
the architecture of the provider host and intended guest:

```bash
cargo build --locked --release \
  -p environment-daemon \
  -p environment-provider-incus
```

The resulting binaries are `target/release/lightspeed-envd` and
`target/release/lightspeed-provider-incus`.

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

The stock image includes common development tools. The daemon runs as an
unprivileged user in `/workspace` and can write there and to its state
directory. Customize the image and service when a workload needs other
software or operating-system permissions.

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
      "disk": "40GiB",
      "idle": {
        "pauseAfterMs": 1800000,
        "stopAfterMs": 14400000
      }
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
Leave out `credentialFile` to run without a credential.

The listener serves the controller on `/control` and the data routes under
`/routes`. Lightspeed derives the data route URL from the controller URL you
register, so the same address serves both. Only when the environment
gateway reaches the data routes under another address, for example through
a separate proxy, register that base with the provider in Lightspeed.

This template pauses VMs after 30 minutes without activity and stops them
after four hours. Leave out `idle` for manual power management. See [Let the
provider manage power](#let-the-provider-manage-power) for how a VM wakes.

The network block denies the metadata-service address in this example. It
is an explicit egress rule, not a complete network policy. The provider also
separates the networks of different universes. Choose additional denied destinations for
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
includes optional settings for the relay, idle checks, an image server, and
application ingress.

## Register the provider and bind a universe

As a Platform Admin, open **Admin → Platform → Environment providers →
Register provider**. Enter:

| Field | Value for the same-host example |
| --- | --- |
| Provider id | `incus-local` |
| Display name (optional) | `Local Incus` |
| Controller endpoint | `ws://127.0.0.1:19090/control` |
| Data route base (optional) | Leave empty: Lightspeed uses `ws://127.0.0.1:19090/routes` |
| Credential (optional) | The contents of the provider credential file |

Choose **Register**. Lightspeed connects to the controller and checks its
handshake. A wrong path or credential refuses the registration; a provider
that is not running yet is accepted and checked again on first use. The
credential is write-only: Lightspeed never shows it again, leaving it empty
when editing the provider keeps the stored one, and **Remove the stored
credential** deletes it. The ID is a stable deployment-wide identifier used
by its universe bindings. For another topology, use the private endpoint
reachable from the environment gateway rather than its own loopback address.
A data route base you enter must use `wss://` when the controller is reached
over TLS.

New registrations are available to every universe by default. To limit access,
switch off **Available to every universe** when registering or editing the
provider, then choose **Bind universe** for each universe that should use it.
The Platform applies changes without restarting the provider.

On first provisioning in a universe, the provider sets up its Incus project
and guest network.

## Provision and verify a VM

In a universe with access to the provider, open **Environments → New
environment**. Choose **Small development VM**, enter a name such as
`Acorn build machine`, and choose **Provision**. To set different pause,
stop, or close thresholds for this VM, choose **Custom idle policy**.

The environment stays provisioning while the VM is created and boots, and
becomes ready once Lightspeed's first connection to its daemon succeeds.
Expand **Details** to inspect the environment ID, provider, template,
provider target, and what the provider last reported. Attach it with **Run
commands** access and select it in a session with **Environments** enabled,
then ask the agent to run `pwd` and a harmless file check.

The provider configures the guest daemon during provisioning. The stock setup
listens privately on port 19091, starts commands in `/workspace`, and keeps
its state in `/var/lib/lightspeed-envd`, outside the agent's working tree. It
uses passive connectivity rather than a daemon registration key. The provider
relays requests to it through the guest network.

Continue with [Processes and jobs](processes-and-jobs.md) for a complete file
and execution check. Finish the test by closing the disposable environment
and waiting for its status to become closed. Closing releases the VM and its
disk, so save any needed output first.

## Let the provider manage power

The provider applies the idle policy chosen when the VM was created.
Running processes, jobs, and open Lightspeed connections keep a VM awake.
Later idle stages can still take effect while it is paused.

A tool call wakes a paused or stopped VM and waits for its daemon. You can
also use **Pause**, **Resume**, and **Stop**. Closing deletes the VM. See
[Power and cleanup](power-and-cleanup.md#set-an-idle-policy) for policy and
manual power controls.

## Update offerings and manage access

Publish a changed image under a new fingerprint and a new template ID. Set
`deprecated: true` on the old template when it should stop appearing for new
environments. Deprecation affects only new environments: existing ones keep
waking, powering, and closing normally. A changed `idle` block applies to
VMs created afterwards. Edit the provider's `templates`
configuration and restart its service: configuration is loaded at startup,
with no hot reload.

Retain old template entries while their environments still need them, because
operations such as ingress enablement consult that configuration. Existing
machines retain their original setup; changing an image does not upgrade their
files or running processes. A VM whose image carries a daemon that speaks
another environment protocol version than the runtime fails when it becomes
ready, with an error naming both versions; recreate it from a current
template.

When access is limited to selected universes, removing a binding prevents new
provisioning and makes that universe's existing machines on the provider
unreachable until access is restored. Those machines remain open. To remove a
provider from the registry, close its environments and remove the bindings
first. Provider-side projects and networks may still need cleanup.

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
| The universe has no templates | Check provider registration, whether the provider is available to the universe, and template deprecation. |
| Incus cannot find the configured image | Use the immutable fingerprint, make it available on the target, or configure a trusted image server. |
| Registering the provider is refused | Check the controller URL path, the credential, and that a data route base you entered uses `wss://` when the controller is reached over TLS. |
| A VM boots but its environment is not ready | Read the environment's state message. Check the guest daemon service, matching binary architecture, provider-to-guest routes, private daemon port, and that the data route base, derived or entered, reaches the provider from the environment gateway. |
| A VM never pauses | Check the template's `idle` block, running processes and jobs, services a command left running, and open sessions using the VM. |
| The environment fails naming two protocol versions | The image's daemon does not match the runtime release. Rebuild the image and recreate the environment. |
| A command cannot write outside the workspace | Inspect the stock service's systemd restrictions and daemon-user permissions. Customize the image deliberately if the task needs more access. |
| A universe's first provisioning fails during network setup | Check Incus permissions, subnet availability, denied-CIDR overlap, and cluster uplink configuration if applicable. |
