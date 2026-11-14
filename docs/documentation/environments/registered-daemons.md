# Run registered daemons

[Bring your own compute](bring-your-own-compute.md) connects one machine by
hand. This guide runs `lightspeed-envd` as a service: as Kubernetes pods
whose identity survives restarts, as interchangeable pods that come and go,
or as a systemd unit on virtual machines. Each section ends in a reference
manifest to adapt.

Every daemon here dials the environment gateway outward with a registration
key, as in the walkthrough. Lightspeed runs no provider, template, or power
logic for these machines: your scheduler or hypervisor starts and stops
them, and Lightspeed records whether each daemon is connected.

## Decide how long an identity lives

A daemon's identity is the key file `daemon-key` in its state directory. The
environment lives exactly as long as that identity, so put the state
directory on storage with the lifetime you want:

| Deployment | Key identity mode | State directory | Daemon on close |
| --- | --- | --- | --- |
| StatefulSet, one environment per pod ordinal | Persistent | A PersistentVolumeClaim per pod | `--on-closed=park` |
| Deployment, interchangeable pods | Ephemeral | An `emptyDir` that ends with the pod | `--on-closed=park` |
| systemd on a VM | Persistent | `/var/lib/lightspeed-envd` on the VM's disk | Exit, and do not restart |

Three rules apply to all of them:

- Never copy a state directory to another pod or machine, bake it into an
  image, or restore it from a snapshot onto a second machine. Two daemons
  with one identity supersede each other, and the older one exits with
  status 5.
- Run one daemon per state directory.
- Keep the state directory outside the agent's working directory. Commands
  running as the daemon's user can still read it, so grant **Run commands**
  only where that is acceptable.

File tools reach every file the daemon's user can read under the daemon's
filesystem root, which is the whole pod or VM by default. They never reach
the state directory, the registration key file, or the `/proc` and `/sys`
filesystems; every other file is readable by a session with **Read files**.
So keep other secrets off the machine. In Kubernetes, set
`automountServiceAccountToken: false`, as the manifests below do: a pod
otherwise carries a token for the cluster API in a file under
`/var/run/secrets`, which a session could read like any other file. Mount a
token only into pods whose agents are meant to call the cluster API, with a
service account bound to the least that needs.

The daemon takes its settings from its process environment and command line
only. It loads no `.env` file, so nothing an agent writes into the working
directory changes how the daemon starts next time.

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

The daemon sends the key only until its first admission, so leaving the
Secret mounted does not affect it. File tools never read the key file, and
they refuse the whole volume when the key is mounted in a directory of its
own, as in the manifests below, so mount nothing else there. A session that
may run commands on the machine can still read the key. Rotate the key with
`lightspeed environment registration-key rotate KEY_ID` when the secret
leaks; admitted daemons keep reconnecting with their identities.

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

`put` makes the key `ci-runners` exist with that policy and answer to exactly
that secret. Running it again changes nothing, so it can be a step of every
deployment; a changed policy is applied, and a different secret retires the
previous one at once, as a rotation does. Declare the key before the daemons
start: a daemon whose key the gateway does not know treats that as a permanent
refusal and exits. Under Kubernetes the pod is restarted and registers once
the key exists, at the cost of restarts in between. The identity mode cannot
be changed afterwards, and a revoked key is never
restored, so either needs a new key id. A supplied secret is `lsrk_` followed
by 32 random bytes encoded as base64url without padding.

Daemons dial `wss://<public host>/environment-gateway/connect`, the route the
public edge publishes; see
[Self-hosting](../deployment/self-hosting.md#configure-the-public-edge).
Inside the same Kubernetes cluster as the runtime, they can instead dial the
environment gateway's Service directly:

1. On the process running the `environment-gateway` role, add the in-cluster
   URL to `LIGHTSPEED_ENVIRONMENT_REGISTRATION_URLS`, for example
   `ws://lightspeed-environment-gateway.lightspeed.svc.cluster.local:18080/environment-gateway/connect`.
   Daemons sign the origin they dial, and the gateway accepts only the
   origins it is configured with.
2. Set `LIGHTSPEED_ENVD_ALLOW_INSECURE_GATEWAY=true` on the daemons when the
   URL is plain `ws://`. The registration key and every data stream then
   cross the cluster network in the clear, so restrict that path with a
   network policy.

The Service can route to multiple environment-gateway replicas. Publish both
`/environment-gateway/connect` and `/environment-gateway/data`; they may land
on different replicas. Configure each gateway's direct peer URL and the shared
gateway token as described in [networking and ingress](networking-and-ingress.md).

## Provide the daemon binary

Each release publishes `lightspeed-envd` as a static x86_64 Linux binary
that runs on any distribution's image. Install it in the image or VM that also carries the
tools your agents need, including Bash for the shell tools.

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
              value: wss://lightspeed.example.com/environment-gateway/connect
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

Lightspeed allocates these IDs when each pod first registers. To choose them
yourself, so that a profile can name a machine before it exists and the IDs
are the same in every deployment, declare the environments; see the next
section.

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

A declaration names three things: the environment ID that profiles and
credential bindings use, the persistent registration key whose daemons may
fill it, and the registration name the daemon presents. That name is the
daemon's `LIGHTSPEED_ENVD_REGISTRATION_NAME`, the pod name in the manifest
above and the host name in the systemd unit below. The daemon itself needs no
other setting.

The environment exists as soon as it is declared. It shows as disconnected,
and a profile can attach it or make it its default at once. A session that
uses it starts; a tool call waits up to two minutes for the daemon and then
fails, as for any persistent machine whose daemon is away.

The first daemon the key admits under the declared name becomes that
environment and keeps the declared ID. From then on it reconnects with its
own identity, as every registered daemon does, and its name is not consulted
again. A daemon whose name matches no declaration registers under an
allocated ID as before, unless the key admits only declared machines.

Declaring is safe to repeat: running the same command again changes nothing,
and a changed display name or metadata is applied. The key and the name of a
declared environment cannot be changed; close it and declare a new one. A
name fills at most one open environment of its key.

### Admit only declared machines

`--declared-only` on a persistent key makes it admit only daemons whose name
matches one of its declared environments. Nothing appears in the universe
that you did not declare, and the order of deployment stops mattering:

| What is deployed first | What happens |
| --- | --- |
| The key and the declarations, then the daemons | Each daemon binds to its environment on its first attempt. |
| The key, then the daemons, then the declarations | With `--declared-only`, each daemon is told to try again and binds once its environment is declared. Without it, a daemon that arrives early registers under an allocated ID and no longer matches its declaration. |
| The daemons, then the key | A daemon whose key the gateway does not know exits. Under Kubernetes the pod restarts until the key exists. |

Use `--declared-only` for every key whose machines you declare.

### Replace a machine that lost its identity

A declared environment is bound to the identity of the daemon that filled
it. A machine that returns without its state directory, for example after its
volume was deleted, has a new identity. It is refused, and keeps retrying,
because the environment is bound to the old one: Lightspeed never lets a
second machine take the place of one that profiles and credentials point at.

Release the environment to let the new machine in:

```bash
lightspeed environment release runner-1
```

The environment forgets its daemon and keeps its ID, its name, its credential
bindings and every profile that references it. The waiting machine binds on
its next attempt, within about fifteen seconds. If the released daemon is
still running, its connection ends and it exits with status 3; when it is
restarted with its registration key it may bind again, so stop the old
machine before you release.

### What a declaration does and does not protect

While a declared environment has no daemon, any machine that holds the key's
secret and presents the declared name can fill it. Treat the key as the
credential for those environments: use one key per StatefulSet or per group
of machines you trust equally, keep `--max-active` at the number of declared
environments, and keep the secret in your secret store. Once a daemon is
bound, neither the secret nor the name is enough to displace it.

A few more rules:

- Only a persistent key can have declared environments.
- A declared environment counts against its key's `--max-active` from the
  moment it is declared.
- **Close when offline for** closes a declared environment whose daemon has
  been away that long. It never closes one that is still waiting for its
  first daemon, or one you released.
- Closing a declared environment frees its name for a new declaration. Its ID
  stays with the closed environment.
- A daemon that already registered under an allocated ID keeps that
  environment. To move the machine under a declared ID, close that
  environment and start the daemon with a fresh state directory.

`lightspeed environment create` and `lightspeed environment register` accept
`--id` in the same way, so provisioned and external environments can have
chosen IDs too.

Scaling down leaves the volumes, and with them the open, disconnected
environments; scaling up again brings them back. When you delete a volume,
its identity is gone for good: close the environment, or let the key's
**Close when offline for** setting close it. If an environment is closed
while its pod runs, `--on-closed=park` keeps the pod running idle instead of
letting Kubernetes restart a spent identity forever. Delete the pod and its
volume to replace it.

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
              value: wss://lightspeed.example.com/environment-gateway/connect
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
LIGHTSPEED_ENVD_GATEWAY_URL=wss://lightspeed.example.com/environment-gateway/connect
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
| Pods connect but commands cannot route | `/environment-gateway/data` does not reach the same environment-gateway process as `/environment-gateway/connect`. |
| Closed environments of an ephemeral fleet accumulate | Closed registered environments stay listed, because a spent identity can never return. Hide them with the default list, which omits closed environments. |

The [environment-variable reference](../reference/environment-variables.md#environment-daemon)
lists every daemon setting.
