# Environment credentials

An environment credential binding connects an environment-variable name to
a stored secret or integration. When Lightspeed starts a process or submits
a job to that environment, it resolves the binding and supplies the value to
the command. The agent can request the operation without placing the secret
in its instructions or tool arguments.

The binding belongs to the environment. Every Lightspeed-started process or
job targeting that machine receives the configured variables, including work
from another session, an inherited sub-agent environment, a bot command
trigger, or a standalone API job. Sharing environment access therefore also
shares access to its injected credentials.

Use a universe owner/admin or platform administrator account for these setup
steps. Start with [an environment you can use](using-environments.md).

## Assign a secret to an existing environment

For a service token used by an Acorn release command:

1. Open **Settings → Secrets → Add secret**.
2. Choose **Secret type → Environment secret**.
3. Enter `Acorn release token` as the **Display name** and put the token in
   **Secret value**. Choose **Add secret**. You can use a disposable sample
   value if you only want to test the binding mechanism.
4. Open **Environments**, find the target machine, and expand **Details**.
5. Under **Secret environment variables**, choose **Assign credential**.
6. Enter `ACORN_RELEASE_TOKEN` as the **Environment variable**, choose the
   stored credential under **Secret source**, and choose **Assign**.

Ask the agent to start a new process in that environment and run this presence
check without printing the value:

```bash
test -n "$ACORN_RELEASE_TOKEN" && printf 'Release credential is available\n'
```

Inspect its output and exit status. This verifies that a nonempty value reached
the process. A subsequent request to the intended service verifies whether
that credential has the right permissions.

Environment secrets preserve opaque multiline values, so the same storage
form can hold material such as an SSH private key. Binding it supplies an
environment variable; it does not automatically create a key file or configure
the program that will use it.

## Choose a credential source

The source picker lists usable credentials already present in the universe:

| Source | What the process receives |
| --- | --- |
| Environment secret | The stored opaque value. |
| Access credential | A bearer value obtained through its grant, including suitable OAuth connections or GitHub installation credentials. |
| Imported coding-agent subscription | The imported token or serialized token set, according to that integration's suggested variable. |
| Model-provider API key | The exportable key from an active provider credential record. |

The public API also supports a direct secret reference. The web form creates
environment secrets through the grant-backed path instead.

A variable name must start with an ASCII letter or underscore and contain
only ASCII letters, digits, and underscores, with at most 128 characters.
Assigning an already bound name replaces its source. This lets you rotate
the credential without changing the command's expected variable name.

There is no override priority between a binding and a command's explicit
`env`: supplying the same name in both is rejected. Remove the explicit
variable from the tool or job request and let the binding supply it. Use
explicit `env` for ordinary non-secret command settings.

Model credentials and environment credentials configure different consumers.
Adding an OpenAI integration does not automatically inject `OPENAI_API_KEY`
into every machine. Binding that variable to a machine does not select or
authenticate the model used by the Lightspeed session. See
[Models and credentials](../using-lightspeed/models-and-credentials.md).

## Share credentials through an environment

Configure credential bindings directly on the Environments page. Profiles may
select an existing environment or inherit a parent's selection, but they do
not create machines or initialize credentials. Sessions using the same machine
receive the same environment bindings. Create separate environments when work
requires different credential access.

## Understand resolution and renewal

Bindings store references rather than secret values. Just before dispatching
the process or admitting the job group, the runtime resolves the sources.
Grants are checked for status, expiry, and audience. Suitable OAuth grants can
refresh through their stored refresh path, and GitHub App installations can
mint installation tokens. Provider API keys and static imported values are
read from their stored records.

That resolution happens when the runtime submits the work to the environment.
A job waiting behind a dependency or queue already has the values supplied
when its group was admitted by the daemon; they are not refreshed again when
its operating-system process eventually starts.
Running processes also keep their original values. Use a credential and task
lifetime appropriate to the operation, and start new work after a credential
change when it needs the new value.

The ordinary environment-injection path does not require the bearer-secret
option **Retrievable by trusted services**. That option serves other explicit
service-leasing use cases. Here, the token broker resolves the binding for
the environment operation.

## Run coding agents with subscription credentials

The subscription integrations supply credentials for coding agents installed
on the environment machine:

| Integration | Suggested variable |
| --- | --- |
| Claude Code (subscription) | `CLAUDE_CODE_OAUTH_TOKEN` |
| Codex (ChatGPT subscription), imported token set | `CODEX_AUTH_JSON` |
| Codex, imported Enterprise access token | `CODEX_ACCESS_TOKEN` |

Create the integration under **Settings → Integrations**, then select it as
the environment's credential source. Install the corresponding coding agent
in the environment separately; assigning a token does not install software.

For the Codex token-set path, the integration details provide a bootstrap
command that writes `auth.json` in the coding agent's configuration directory.
Run that setup inside the credentialed process or job before invoking the
coding agent. The daemon does not write the file automatically. The resulting
file is another credential copy with the machine's own lifetime.

Imported subscription credentials are static stored grants. Lightspeed does
not turn them into refreshable session-model connections, and it does not
synchronize changes a coding agent makes to its local auth file back into the
stored integration. Reimport and reassign when the stored credential expires
or becomes stale. Avoid repeatedly overwriting a local token set that the
coding tool has already refreshed.

The Platform assignment form rejects pairing `CLAUDE_CODE_OAUTH_TOKEN` with
`ANTHROPIC_API_KEY` or `ANTHROPIC_AUTH_TOKEN` on the same environment, because
those variables select a competing authentication path. This is a Platform
assignment check; commands can also inherit variables from the daemon's
launch environment, which should be configured consistently.

## Rotate or remove an assignment

The current static-secret UI does not edit an existing secret value in place.
Create a replacement credential, assign the same environment-variable name
to it, and verify a new process. Revoke the old credential once its other
users no longer need it. Custom credential IDs must be unique; a revoked ID
cannot be reused for the replacement.

Choose **Unassign variable** to remove a binding while retaining its stored
credential. Removing a binding or revoking a grant affects future resolution;
it cannot remove a value already held by a running process or admitted job.
It also does not erase files or other copies the program created.

Closing an environment is a machine-lifecycle action, not revocation of its
universe integrations. The effects on the machine depend on its source; see
[Power and cleanup](power-and-cleanup.md).

## Use the CLI or API

With the [CLI connection settings](../using-lightspeed/sessions-and-runs.md#continue-from-the-cli)
configured, bind a stored grant by reference:

```bash
target/debug/lightspeed env credentials bind "<environment-id>" \
  --env-name ACORN_RELEASE_TOKEN --grant-id "<credential-id>"
target/debug/lightspeed env credentials list "<environment-id>"
```

Remove that assignment with:

```bash
target/debug/lightspeed env credentials unbind "<environment-id>" \
  --env-name ACORN_RELEASE_TOKEN
```

The params for `environments/credentials/bind` use the same reference model:

```json
{
  "environmentId": "<environment-id>",
  "envName": "ACORN_RELEASE_TOKEN",
  "source": {
    "type": "authGrant",
    "grantId": "<credential-id>"
  }
}
```

For `authProviderCredential`, the `providerId` field names the credential
record, commonly `model:openai`, rather than the session's model route ID
`openai`. The CLI uses `--provider-id` for that source and `--secret-id` for a
direct secret. Supply only one source. Exact types and list/unbind methods are
in the [API reference](../../../crates/api/contract/api-reference.md).

## Keep the exposure boundary clear

The runtime supplies secret material outside model-authored tool arguments
and session activation events. The daemon keeps job credential values in
memory and persists their variable names rather than values in job records.
The operating-system process nevertheless receives the plaintext values and
can read or transmit them using its permissions.

Captured output masks literal secret values, but this is limited byte
replacement within output chunks. It does not reliably cover transformed
values, values split across chunks, files, or network requests. Use presence
checks like the one above rather than printing credentials into the transcript.

## If a command cannot use its credential

| Symptom | What to check |
| --- | --- |
| The variable is absent | Check the target environment and exact name, then start a new process rather than continuing one already running. |
| Submission reports an environment-variable collision | Remove the bound name from explicit command or job `env`. |
| Credential resolution fails before execution | Check that the referenced source is active, unexpired, and suitable for the requested audience. |
| A profile change did not rotate the machine's token | Profiles do not manage credentials. Change the environment binding directly. |
| A queued job uses an old token | Its values were resolved at admission. Submit new work after updating the binding. |
| An imported subscription stopped working | Reimport a current credential and update its binding; the static imported record is not refreshed by core. |
| Unassigning did not remove an on-disk login | A program or bootstrap wrote a separate copy. Manage that file and any running processes explicitly. |
