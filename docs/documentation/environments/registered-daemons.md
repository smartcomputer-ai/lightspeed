# Run registered daemons

Run `lightspeed-envd` as a Kubernetes workload or a systemd service when you
want machines to connect automatically. Each daemon connects outbound to the
environment gateway. Your scheduler or hypervisor controls the machines'
power and lifetime. For a one-machine walkthrough, see [Bring your own
compute](bring-your-own-compute.md).

## Decide how long an identity lives

Store each daemon's state directory for as long as you want the machine to
keep the same identity:

| Deployment | Key identity mode | State directory | Daemon on close |
| --- | --- | --- | --- |
| StatefulSet, one environment per pod ordinal | Persistent | A PersistentVolumeClaim per pod | `--on-closed=park` |
| Deployment, interchangeable pods | Ephemeral | An `emptyDir` that ends with the pod | `--on-closed=park` |
| systemd on a VM | Persistent | `/var/lib/lightspeed-envd` on the VM's disk | Exit, and do not restart |

Give each daemon its own state directory and keep it outside the working
directory. If two daemons share one identity, the newer connection replaces
the older. Keep state directories out of shared images and volumes.

File tools protect the daemon state and registration key file. Other readable
secrets are accessible inside the daemon's filesystem root, including the
Kubernetes service account token if you mount it. The examples below set
`automountServiceAccountToken: false`. Commands have the daemon user's
operating-system permissions. See [Agent and tool
access](../access-and-security/agent-and-tool-access.md).

## Prepare the key and the gateway URL

Mint one registration key per group of daemons. For a StatefulSet of three
pods:

```bash
lightspeed environment registration-key create \
  --display-name "CI runners" --mode persistent --max-active 5
```

Set `--max-active` to the replica count plus room for a rollout, so a
leaked key cannot add more machines than you expect. For a persistent key,
`--offline-close-after` (for example `12h` or `30d`) closes environments whose daemon stays away
that long, which suits a fleet whose volumes you delete when scaling down.
Store the secret printed once in a Kubernetes Secret, or in a file readable
only by the daemon's user on a VM:

```bash
kubectl -n lightspeed-runners create secret generic lightspeed-envd-key \
  --from-file=registration-key=./registration-key
```

The daemon sends the key only on its first admission. Mount the key by itself
in a dedicated directory, as in the manifests below. File tools exclude that
mount, and commands running as the daemon's user can read it. Rotate an
exposed secret with `lightspeed environment registration-key rotate KEY_ID`.
Admitted daemons keep reconnecting with their own identities.

### Declare the key from a secret store

Minting returns a secret you then have to carry into the cluster. A
deployment pipeline can go the other way: let the secret store hold the
secret, mount it into the daemons, and declare the key from the same value.
No step has to read a secret out of Lightspeed.

```bash
lightspeed environment registration-key generate-secret > registration-key   # or generate it in the secret store
kubectl -n lightspeed-runners create secret generic lightspeed-envd-key \
  --from-file=registration-key=./registration-key

lightspeed environment registration-key put ci-runners \
  --display-name "CI runners" --mode persistent --max-active 5 \
  --secret-file ./registration-key
```

`put` creates or updates the key's policy and secret. It is safe to repeat
and immediately replaces the previous secret when it changes. Declare the
key before starting the daemons. A daemon that presents an unknown key exits
and relies on its supervisor to restart it. Use a new key ID to change the
identity mode or replace a revoked key.

Daemons dial `wss://<public host>/environments/connect`, the route the
public edge publishes; see
[Self-hosting](../deployment/self-hosting.md#configure-the-public-edge).
Inside the same Kubernetes cluster as the runtime, they can instead dial the
environment gateway's Service directly:

1. On the process running the `environment-gateway` role, add the in-cluster
   URL to `LIGHTSPEED_ENVIRONMENT_REGISTRATION_URLS`, for example
   `ws://lightspeed-environment-gateway.lightspeed.svc.cluster.local:18083/environments/connect`.
   Daemons sign the origin they dial, and the gateway accepts only the
   origins it is configured with.
2. Set `LIGHTSPEED_ENVD_ALLOW_INSECURE_GATEWAY=true` on the daemons when the
   URL is plain `ws://`. The registration key and every data stream then
   cross the cluster network in the clear, so restrict that path with a
   network policy.

The Service can route to multiple environment-gateway replicas. Publish both
`/environments/connect` and `/environments/data`; they may land
on different replicas. Configure each gateway's direct peer URL and the shared
gateway token as described in [networking and ingress](networking-and-ingress.md).

## Provide the daemon binary

Use the daemon from the same Lightspeed release as your runtime. Releases
include x86_64 Linux and Windows binaries. These Kubernetes and systemd
examples use the static Linux binary. Install it alongside the tools your
agents need, including Bash for shell tools.

Automatic upgrade is off unless you set `LIGHTSPEED_ENVD_AUTO_UPGRADE`. Keep
it off in containers: it replaces the executable in place, which a
read-only root filesystem forbids and a new pod would undo. Upgrade by
rolling out an image with the new daemon instead. When the workload image
should not contain the daemon, or its root filesystem is read-only, copy the
binary from an init container into an `emptyDir`, as the Deployment example
below does.

## Run a StatefulSet with persistent identities

Each pod keeps its identity on its own volume. A rolling restart, a node
drain, or a crash brings the pod back with the same volume, so it reconnects
as the same environment. While a pod is away its environment stays open and
shows as disconnected, and a tool call waits up to two minutes for it to
return.

```yaml
apiVersion: apps/v1
kind: StatefulSet
metadata:
  name: lightspeed-runner
  namespace: lightspeed-runners
spec:
  serviceName: lightspeed-runner
  replicas: 3
  podManagementPolicy: Parallel
  selector:
    matchLabels:
      app: lightspeed-runner
  template:
    metadata:
      labels:
        app: lightspeed-runner
    spec:
      # No Kubernetes API token in the pod: file tools could read it.
      automountServiceAccountToken: false
      terminationGracePeriodSeconds: 30
      securityContext:
        runAsNonRoot: true
        runAsUser: 10001
        runAsGroup: 10001
        fsGroup: 10001
      containers:
        - name: envd
          # Your tools plus lightspeed-envd at /usr/local/bin.
          image: registry.example.com/lightspeed-runner:2026.10.1
          command:
            - /bin/sh
            - -c
            - mkdir -p /data/workspace && exec /usr/local/bin/lightspeed-envd
          env:
            - name: POD_NAME
              valueFrom:
                fieldRef:
                  fieldPath: metadata.name
            - name: LIGHTSPEED_ENVD_GATEWAY_URL
              value: wss://lightspeed.example.com/environments/connect
            - name: LIGHTSPEED_ENVD_REGISTRATION_KEY_FILE
              value: /etc/lightspeed-envd/registration-key
            - name: LIGHTSPEED_ENVD_REGISTRATION_NAME
              value: $(POD_NAME)
            - name: LIGHTSPEED_ENVD_REGISTRATION_METADATA
              value: '{"k8s.pod":"$(POD_NAME)"}'
            # The daemon key lives here: on the pod's own volume.
            - name: LIGHTSPEED_ENVD_STATE_DIR
              value: /data/state
            - name: LIGHTSPEED_ENVD_CWD
              value: /data/workspace
            - name: LIGHTSPEED_ENVD_ON_CLOSED
              value: park
          securityContext:
            allowPrivilegeEscalation: false
          volumeMounts:
            - name: data
              mountPath: /data
            - name: registration-key
              mountPath: /etc/lightspeed-envd
              readOnly: true
      volumes:
        - name: registration-key
          secret:
            secretName: lightspeed-envd-key
            defaultMode: 0440
  volumeClaimTemplates:
    - metadata:
        name: data
      spec:
        accessModes: ["ReadWriteOnce"]
        resources:
          requests:
            storage: 20Gi
```

Each pod registers under its pod name and records it as metadata, so you can
find the environment ID for a profile:

```bash
lightspeed environment list --registration-key KEY_ID --metadata k8s.pod=lightspeed-runner-0
```

Attach those IDs in profiles explicitly. The ID stays the same for as long as
the pod's volume exists.

Scaling down leaves the volume and its disconnected environment in place.
Scaling back up with the volume reconnects that identity. If you retire the
volume, close the environment or let the key's **Close when offline for**
setting close it. When a running pod's environment closes, `--on-closed=park`
keeps the pod idle. Delete the pod and its volume to replace the identity.

Lightspeed allocates these IDs when each pod first registers. To choose them
before the pods exist, declare the environments.

## Declare fixed machines before their daemons register

A registered environment normally appears when its daemon first connects,
under an ID Lightspeed allocates. For a small set of long-lived machines you
can instead declare each environment in advance, with an ID you choose:

```bash
lightspeed environment registration-key put ci-runners \
  --display-name "CI runners" --mode persistent --max-active 3 --declared-only \
  --secret-file ./registration-key

for i in 0 1 2; do
  lightspeed environment declare "runner-$i" \
    --key ci-runners --registration-name "lightspeed-runner-$i"
done
```

The environment exists immediately and shows as disconnected. Profiles and
credential bindings can reference its chosen ID before the daemon arrives.
The daemon supplies `LIGHTSPEED_ENVD_REGISTRATION_NAME`, which is the pod
name in the StatefulSet above and the host name in the systemd unit below.
The first daemon admitted under the key and declared name takes that ID.
After binding, it reconnects with its own identity.

You can repeat a declaration to update its display name or metadata. To use a
different key or registration name, close it and declare a new environment.
You can also include declarations in a [universe
document](../deployment/configuration-as-code.md#environment-entries).

### Admit only declared machines

Create the persistent key with `--declared-only` before starting the daemons.
If a machine arrives before its declaration, it retries until the matching
name exists. A key that permits undeclared machines gives an early daemon an
allocated ID. To move such a machine to a declared ID, close its environment
and start it with a fresh state directory.

### Replace a machine that lost its identity

A machine that loses its state directory has a new identity. It keeps
retrying while the declared environment is bound to the old one. Stop the
old daemon and release the environment to let the replacement in:

```bash
lightspeed environment release runner-1
```

The release ends the old daemon's connection and preserves the environment's
ID, name, credential bindings, and profile references. The waiting machine
binds on its next attempt. Keep the old daemon stopped, since it could fill
the declaration again if it starts with the key.

### Manage declaration capacity and access

Any daemon with the registration secret and the matching name can fill an
unbound declaration. Use one key per group of machines you trust equally.
Each declaration counts against `--max-active` as soon as you create it.
**Close when offline for** applies to bound machines, so an unbound
declaration can keep waiting. Closing a declaration frees its registration
name for a new declaration with a new ID.

## Run a Deployment with ephemeral identities

For interchangeable workers, give each pod a fresh identity in an `emptyDir`
and use an ephemeral key. A pod that stops deliberately closes its
environment at once; one that disappears without saying goodbye is closed
after the key's disconnect grace period. Each new pod is a new environment
with a new ID, so find these environments by key or metadata rather than
naming them in profiles.

This example also keeps the workload's root filesystem read-only and copies
the daemon from an init container:

```yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: lightspeed-sandbox
  namespace: lightspeed-runners
spec:
  replicas: 4
  selector:
    matchLabels:
      app: lightspeed-sandbox
  template:
    metadata:
      labels:
        app: lightspeed-sandbox
    spec:
      automountServiceAccountToken: false
      terminationGracePeriodSeconds: 30
      securityContext:
        runAsNonRoot: true
        runAsUser: 10001
        runAsGroup: 10001
        fsGroup: 10001
      initContainers:
        - name: install-envd
          # A small image holding /lightspeed-envd and a cp binary.
          image: registry.example.com/lightspeed-envd:2026.10.1
          command: ["cp", "/lightspeed-envd", "/opt/lightspeed/bin/lightspeed-envd"]
          volumeMounts:
            - name: envd-bin
              mountPath: /opt/lightspeed/bin
      containers:
        - name: workload
          image: registry.example.com/sandbox-tools:2026.10.1
          command: ["/opt/lightspeed/bin/lightspeed-envd"]
          env:
            - name: POD_NAME
              valueFrom:
                fieldRef:
                  fieldPath: metadata.name
            - name: LIGHTSPEED_ENVD_GATEWAY_URL
              value: wss://lightspeed.example.com/environments/connect
            - name: LIGHTSPEED_ENVD_REGISTRATION_KEY_FILE
              value: /etc/lightspeed-envd/registration-key
            - name: LIGHTSPEED_ENVD_REGISTRATION_NAME
              value: $(POD_NAME)
            # The identity ends with the pod.
            - name: LIGHTSPEED_ENVD_STATE_DIR
              value: /var/lib/lightspeed-envd
            - name: LIGHTSPEED_ENVD_CWD
              value: /workspace
            - name: LIGHTSPEED_ENVD_ON_CLOSED
              value: park
          securityContext:
            readOnlyRootFilesystem: true
            allowPrivilegeEscalation: false
          volumeMounts:
            - name: envd-bin
              mountPath: /opt/lightspeed/bin
              readOnly: true
            - name: state
              mountPath: /var/lib/lightspeed-envd
            - name: workspace
              mountPath: /workspace
            - name: tmp
              mountPath: /tmp
            - name: registration-key
              mountPath: /etc/lightspeed-envd
              readOnly: true
      volumes:
        - name: envd-bin
          emptyDir: {}
        - name: state
          emptyDir: {}
        - name: workspace
          emptyDir: {}
        - name: tmp
          emptyDir: {}
        - name: registration-key
          secret:
            secretName: lightspeed-envd-sandbox-key
            defaultMode: 0440
```

Mint its key with `--mode ephemeral` and a `--max-active` that covers the
replicas plus the rollout surge. An `emptyDir` survives a container restart
inside the same pod, so a crashed daemon comes back as the same environment
if it returns within the grace period. Once that environment is closed, the
pod parks; delete it and the Deployment starts a replacement with a new
identity.

## Run a systemd unit on a VM

On an enterprise VM, run the daemon as a dedicated user with outbound
connections only. The VM keeps its identity on its own disk, so a reboot
reconnects as the same environment. A VM someone powers off simply
disconnects: tool calls wait up to two minutes, then fail until it returns.
Lightspeed never tries to start it.

Install the binary at `/usr/local/bin/lightspeed-envd`, create the
`lightspeed-envd` user and the working directory, and put the key in
`/etc/lightspeed-envd/registration-key`, owned by that user with mode
`0600`. Then create `/etc/lightspeed-envd/envd.env`:

```bash
LIGHTSPEED_ENVD_GATEWAY_URL=wss://lightspeed.example.com/environments/connect
LIGHTSPEED_ENVD_REGISTRATION_KEY_FILE=/etc/lightspeed-envd/registration-key
LIGHTSPEED_ENVD_STATE_DIR=/var/lib/lightspeed-envd
LIGHTSPEED_ENVD_CWD=/srv/lightspeed/workspace
```

and `/etc/systemd/system/lightspeed-envd.service`:

```ini
[Unit]
Description=Lightspeed environment daemon
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=lightspeed-envd
Group=lightspeed-envd
EnvironmentFile=/etc/lightspeed-envd/envd.env
Environment=LIGHTSPEED_ENVD_REGISTRATION_NAME=%H
ExecStart=/usr/local/bin/lightspeed-envd
Restart=always
RestartSec=5
# 3: registration refused for good; 4: environment closed, identity spent;
# 5: another daemon with the same identity took over. Restarting cannot help.
RestartPreventExitStatus=3 4 5
StateDirectory=lightspeed-envd
StateDirectoryMode=0700
WorkingDirectory=/srv/lightspeed/workspace
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ReadWritePaths=/srv/lightspeed/workspace

[Install]
WantedBy=multi-user.target
```

Enable it with `systemctl enable --now lightspeed-envd`. The VM registers
under its host name. `systemctl stop` tells the gateway the daemon is
stopping and exits with status 0. `RestartPreventExitStatus` leaves the unit
failed instead of restarting it when the environment was closed or the key
refused it; `journalctl -u lightspeed-envd` names the reason. A replacement
needs a fresh state directory.

`ProtectSystem=strict` keeps the executable read-only, so upgrade the binary
with your package or configuration management. To let the daemon upgrade
itself when the gateway moves to a new protocol version, install it in a
directory listed in `ReadWritePaths` and set
`LIGHTSPEED_ENVD_AUTO_UPGRADE=true`.

## If a fleet misbehaves

| Symptom | What to check |
| --- | --- |
| A pod restarts in a loop with status 3 | The key is invalid, revoked, or expired, or the gateway speaks another protocol version. The log names the reason. |
| New pods never register, and their logs report the key's capacity | The key is at its active environment limit. The daemon keeps retrying; raise the limit with **Edit** or close environments it admitted. |
| A daemon keeps retrying and logs that no environment is declared for its registration name | Its key admits only declared machines. Declare an environment with that name, or correct `LIGHTSPEED_ENVD_REGISTRATION_NAME`. |
| A daemon keeps retrying and logs that its environment is bound to another daemon identity | The machine lost its state directory, or two machines use one name. Release the environment for a replacement; give a second machine its own name and declaration. |
| A declared environment stays disconnected while its machine registers under another ID | The daemon's name does not match the declaration, or it registered before the declaration existed. Close the stray environment, fix the name, and restart the daemon with a fresh state directory. |
| A pod logs that its environment is closed and stays idle | The environment was closed and the pod parked. Delete the pod, and for a StatefulSet its volume. |
| Two pods take turns connecting, one exits with status 5 | Both use one state directory. Give every pod its own volume. |
| Registration is refused because the signature does not verify | The URL the daemon dials is not the public base URL or one of `LIGHTSPEED_ENVIRONMENT_REGISTRATION_URLS`. |
| The daemon refuses a `ws://` gateway URL | Set `LIGHTSPEED_ENVD_ALLOW_INSECURE_GATEWAY=true`, or use `wss://`. |
| Pods connect but commands cannot route | Check that the edge upgrades WebSockets for both `/environments/connect` and `/environments/data`. With replicas, check direct peer URLs and the shared gateway token. |
| Closed environments of an ephemeral fleet accumulate | Closed registered environments stay listed, because a spent identity can never return. Hide them with the default list, which omits closed environments. |

The [environment-variable reference](../reference/environment-variables.md#environment-daemon)
lists every daemon setting.
