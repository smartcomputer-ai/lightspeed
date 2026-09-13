# Bring your own compute

Run `lightspeed-envd` on a machine you control, and its filesystem and processes
can become available to a Lightspeed session. The daemon connects outward to
the environment gateway, so the machine can sit behind NAT without exposing an
inbound daemon port.

This walkthrough connects a machine, selects it for a session, and runs `pwd`
to verify the connection. It uses persistent registration so that restarting
the daemon brings back the same environment.

## Prepare the machine

You need a running Lightspeed installation and an account that can manage the
universe's environments and sessions. The [local quickstart](../getting-started/quickstart.md)
provides both. For a deployed installation, the operator must expose the
environment registration routes described in
[Self-hosting](../deployment/self-hosting.md#configure-the-public-edge).

Use a machine and operating-system account whose permissions fit the work you
want the agent to do. Commands execute as the daemon's user. A working directory
does not confine those commands; use an appropriate VM, container, or OS user
when the work needs isolation. Install Bash for the shell tools used by the
default OpenAI configuration.

Build the daemon from the same source revision as your runtime. In the
Lightspeed checkout:

```bash
cargo build --locked -p environment-daemon
```

The repository builds the daemon for Linux x86_64 and checks native
development builds on Apple Silicon macOS. Build on the target platform rather
than copying a binary between operating systems or architectures.

Record its absolute path before leaving the checkout:

```bash
LIGHTSPEED_ENVD_BIN="$(pwd)/target/debug/lightspeed-envd"
```

Create a directory for this machine's work and a private directory for its
identity. Run the following commands in the same shell:

```bash
mkdir -p "$HOME/lightspeed-machine/workspace" "$HOME/lightspeed-machine/state"
chmod 700 "$HOME/lightspeed-machine/state"
cd "$HOME/lightspeed-machine"
umask 077
```

The daemon loads a `.env` from its process working directory or ancestors at
startup. Check that those locations contain only configuration intended for
this daemon. The `--cwd` option below controls where agent commands start; it
does not change the daemon's initial `.env` lookup. The commands also inherit
the daemon user's process environment.

## Admit the machine to a universe

In the web app, open the intended universe and choose **Environments →
Registration key**. Enter `My machines` as the group name, select
**Persistent** for its identity mode, and set **Active environment limit** to
`1` for this walkthrough.

Choose **Mint key** and save the secret displayed once. Using a local text
editor, put that secret alone in:

```text
~/lightspeed-machine/registration-key
```

Set its permissions:

```bash
chmod 600 "$HOME/lightspeed-machine/registration-key"
```

The registration key is permission to admit a new machine into this universe.
It is separate from the API key a client might use to call Lightspeed. After
registration, the daemon has its own identity for reconnecting.

## Start the daemon

For the local quickstart, run:

```bash
"$LIGHTSPEED_ENVD_BIN" \
  --gateway-url ws://127.0.0.1:18080/environment-gateway/connect \
  --registration-key-file "$HOME/lightspeed-machine/registration-key" \
  --registration-name "My workstation" \
  --registration-receipt "$HOME/lightspeed-machine/receipt.json" \
  --cwd "$HOME/lightspeed-machine/workspace" \
  --state-dir "$HOME/lightspeed-machine/state"
```

For a remote installation, replace the gateway URL with its public endpoint,
for example `wss://lightspeed.example.com/environment-gateway/connect`.
Plain `ws://` is accepted only for loopback connections. Leave the daemon
running in this terminal.

The daemon opens an outbound control connection. When a worker needs a data
connection, the daemon opens another outbound connection for that route.
This command does not start a passive listener on the machine.

Wait for `receipt.json` to appear, then reload the **Environments** page and
confirm **My workstation** is ready. The receipt records the assigned
environment and daemon identifiers, without the registration secret. Keep the
state directory: it contains the daemon's private identity key.

Once registration succeeds, you can remove the admission secret from this
machine in another terminal:

```bash
rm "$HOME/lightspeed-machine/registration-key"
```

The registration key itself can admit more identities while it is valid and
within its limits. Removing this local file does not revoke that key.

## Run a command from a session

Open a session with a working model, such as the one from the
[first-agent walkthrough](../getting-started/first-agent.md). When the session
is idle, open the sliders button labeled **Session settings**. Enable
**Environments**, attach **My workstation** with **Exec** access, choose it
under **Active environment**, and choose **Apply setup**.

You do not need to enable model-driven selection or background jobs to use
this selected machine. Send:

```text
Run pwd in the active environment and show the result. Do not change any files.
```

Inspect the process tool's result. It should report the `workspace` directory
you created on the daemon machine. The displayed tool name depends on the
model, so look for the operation and its result rather than one particular
tool name.

If you used the release-editor session, its VFS files remain in the linked
Lightspeed workspace. Selecting this machine does not copy those files into
the directory printed by `pwd`.

## Stop and reconnect

Press Ctrl+C in the daemon terminal. With persistent registration, the
environment becomes offline and remains in the universe; reload
**Environments** to inspect its status. To reconnect, use the same state
directory and omit the deleted registration-key file:

```bash
"$LIGHTSPEED_ENVD_BIN" \
  --gateway-url ws://127.0.0.1:18080/environment-gateway/connect \
  --registration-receipt "$HOME/lightspeed-machine/receipt.json" \
  --cwd "$HOME/lightspeed-machine/workspace" \
  --state-dir "$HOME/lightspeed-machine/state"
```

Use the same remote `wss://` URL if you changed it in the first command. In a
new shell, set `LIGHTSPEED_ENVD_BIN` to the binary's absolute path again. The
environment returns under its existing identity. Reload **Environments** and
repeat the `pwd` request to verify access.

For short-lived machines, a registration key can instead use an ephemeral
identity mode. Those environments close after their disconnect grace period.
Once an environment is closed, its daemon identity cannot register again;
a replacement needs a fresh state directory and a valid registration key.

## Finish the walkthrough

To remove this machine's access permanently, clear the session's active
environment, stop the daemon, and close its environment through the CLI or
API described in [Power and cleanup](power-and-cleanup.md#close-a-machine-deliberately).
The current web app has no individual close button for registered or external
environments. Revoke the registration key when you no longer need it to admit
machines.

Revoking a key alone blocks new identities. Already admitted environments
can continue reconnecting until you close them. Closing a registered
environment does not shut down or delete the underlying computer.

## Direct attachment for local development

The full local launcher also offers a shorter, direct connection path. On the
next launcher startup, omit `--no-envd`:

```bash
./dev.sh
```

It starts a passive daemon at `ws://127.0.0.1:19091/`, using
`.lightspeed-dev/envd/workspace` in the checkout as its working directory.
Open **Environments → Attach local daemon**, keep that endpoint, and choose
**Register**. Then select the environment for a session as above.

This creates an external environment: the runtime connects to the daemon.
The passive listener has no built-in authentication or TLS, so keep this
shortcut on loopback or a protected network. In the source-based local stack,
the runtime and daemon run on the same host; in a container deployment,
`127.0.0.1` refers to the container making the connection.

## If the connection fails

| Symptom | What to check |
| --- | --- |
| The environment never appears | Check the registration secret, its universe and limits, and that the daemon can reach the gateway. |
| A remote WebSocket connection fails | Use `wss://` and ensure both `/environment-gateway/connect` and `/environment-gateway/data` reach the environment gateway through a WebSocket-capable proxy. |
| Restart fails after deleting the key file | Remove `--registration-key-file` from the command and retain the original `--state-dir`. |
| The daemon reports incompatible protocol versions | Use daemon and runtime builds from the same release. `lightspeed-envd --print-build` reports daemon build and protocol information. |
| The agent has no process tool | Enable **Environments**, select the active environment, and apply the setup to the actual session. |
| A command cannot start or find a program | Check the daemon user's permissions, installed programs, Bash, and working directory on the machine. |
| A closed environment cannot reconnect | Its identity is spent. Create a replacement with a new state directory and registration key rather than reusing the closed identity. |

Continue with [Using environments](using-environments.md) for profiles and
sharing, then [Processes and jobs](processes-and-jobs.md) for a complete
execution example. Detailed daemon settings are in the
[configuration reference](../reference/environment-variables.md#environment-daemon).
