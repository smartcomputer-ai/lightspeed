# Environment credentials

An environment credential binding connects an environment-variable name to
a stored credential. When Lightspeed starts a process or submits
a job to that environment, it resolves the binding and supplies the value to
the command. The agent can request the operation without placing the secret
in its instructions or tool arguments.

The binding belongs to the environment. Every Lightspeed-started process or
job targeting that machine receives the configured variables, including work
from another session, an inherited sub-agent environment, a bot command
trigger, or a standalone API job. Sharing environment access therefore also
shares access to its injected credentials.

Because the process receives the plain value, binding a credential is a way
to read it. Only two kinds of source can be bound: a credential created as
**retrievable**, and the API key of a model provider. A brokered credential
is one whose value never leaves the runtime, so it cannot be bound. See
[Choose a credential source](#choose-a-credential-source).

Use an Operator or Admin account for these setup steps. Contributors can use
the resulting environment in sessions they control, including its injected
credentials. Start with [an environment you can use](using-environments.md).

## Assign a secret to an existing environment

For a service token used by an Acorn release command:

1. Open **Credentials → Add credential → Paste a token or secret**.
2. Choose **Secret type → Environment secret**.
3. Enter `Acorn release token` as the **Display name** and put the token in
   **Secret value**. Choose **Add credential**. You can use a disposable sample
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

The source picker lists the credentials in the universe that can be bound:

| Source | What the process receives |
| --- | --- |
| Environment secret | The stored opaque value. |
| Retrievable access credential | A bearer value obtained through its grant: a token created with the retrievable option, or a GitHub installation credential, which supplies a short-lived installation token. |
| Imported coding-agent subscription | The imported token or serialized token set, according to that integration's suggested variable. |
| Model-provider API key | The stored key of an active model API key provider. |

Every credential has an exposure, chosen when it is created and fixed
afterwards:

- A **brokered** credential stays inside the runtime, which attaches it to
  its own outbound requests, such as calls to an MCP server or a header of a
  model endpoint. Nothing returns its value.
- A **retrievable** credential can also be bound into an environment, and
  leased by trusted services for bot webhook signatures and authenticated
  polls.

Binding requires a retrievable credential, and the runtime checks this both
when the binding is made and each time it is resolved for a process or job.
Environment secrets, imported coding-agent subscriptions and GitHub
installation credentials granted on the **Credentials** page are created
retrievable. For a pasted token, select **Retrievable by environments and
trusted services** when you create it. A brokered credential does not appear
in the picker, and the dialog says how many were left out. To use one in an
environment, add a new retrievable credential with the same value; the
exposure of an existing credential cannot be changed.

A model provider's API key is the one case where a plain value reaches a
machine without the retrievable option: the key exists to call the model
provider, and binding it lets commands on the machine make the same calls.
No other provider credential can be bound. In particular a GitHub App's
private key cannot: bind a GitHub installation credential of that App
instead, and the process receives an installation token that expires within
the hour. A credential named by an MCP server header or a model endpoint
header is attached by the runtime to its own request and does not need to be
retrievable.

There is no source that names a stored secret directly. Every bound value is
a credential or a model provider key, so the rules above cover every binding.

A variable name must start with an ASCII letter or underscore and contain
only ASCII letters, digits, and underscores, with at most 128 characters.
Assigning an already bound name replaces its source. This lets you rotate
the credential without changing the command's expected variable name.

There is no override priority between a binding and a command's explicit
`env`: supplying the same name in both is rejected. Remove the explicit
variable from the tool or job request and let the binding supply it. Use
explicit `env` for ordinary non-secret command settings.

Model credentials and environment credentials configure different consumers.
Adding an OpenAI provider on **Models** does not automatically inject
`OPENAI_API_KEY` into every machine. Binding that variable to a machine does not select or
authenticate the model used by the Lightspeed session. See
[Models and credentials](../using-lightspeed/models-and-credentials.md).

## Share credentials through an environment

Configure credential bindings directly on the Environments page. Profiles may
attach existing environments or, for sub-agents, inherit a parent's
selection. Sessions using the same machine receive the same environment
bindings. Create separate environments when work requires different credential
access.

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

Environment injection requires a credential created with **Retrievable by
environments and trusted services**, because the resolved value is handed to
the process. A binding whose credential is brokered, or whose provider is not
a model API key provider, fails at this step with a message that names the
variable and the credential; no command runs until the binding is changed or
removed.

## Run coding agents with subscription credentials

The subscription integrations supply credentials for coding agents installed
on the environment machine:

| Integration | Suggested variable |
| --- | --- |
| Claude Code (subscription) | `CLAUDE_CODE_OAUTH_TOKEN` |
| Codex (ChatGPT subscription), imported token set | `CODEX_AUTH_JSON` |
| Codex, imported Enterprise access token | `CODEX_ACCESS_TOKEN` |

Open **Models → Add provider**, choose the subscription entry, and import its
credential. Then select it as the environment's credential source. Install
the corresponding coding agent on the machine separately.

For the Codex token-set path, the provider details provide a bootstrap
command that writes `auth.json` in the coding agent's configuration directory.
Run that setup inside the credentialed process or job before invoking the
coding agent. The daemon does not write the file automatically. The resulting
file is another credential copy with the machine's own lifetime.

Imported subscription credentials are static stored grants. Lightspeed does
not turn them into refreshable session-model connections, and it does not
synchronize changes a coding agent makes to its local auth file back into the
stored credential. Reimport and reassign when the stored credential expires
or becomes stale. Avoid repeatedly overwriting a local token set that the
coding tool has already refreshed.

The Platform assignment form rejects pairing `CLAUDE_CODE_OAUTH_TOKEN` with
`ANTHROPIC_API_KEY` or `ANTHROPIC_AUTH_TOKEN` on the same environment, because
those variables select a competing authentication path. This is a Platform
assignment check; commands can also inherit variables from the daemon's
launch environment, which should be configured consistently.

## Rotate or remove an assignment

To change a secret's value, choose **Replace value** on its row in
**Credentials** and paste the new value. The credential keeps its ID, so
every variable bound to it supplies the new value to the next process or job
without any binding changing. Processes already running and jobs already
admitted keep the old value. Environment secrets and tokens can be replaced;
OAuth and GitHub App credentials renew themselves instead. From the CLI, use
`lightspeed credential replace ID --token-stdin`, which keeps the
credential's settings. A pipeline that holds the value in a secret store can
declare the whole credential with `lightspeed credential put ID --token-env
VARIABLE`: it creates the credential when missing, applies changed settings,
replaces the token only when it differs, and changes nothing when repeated.

To move a variable to a different credential instead, assign the same
environment-variable name to that credential and verify a new process. Revoke
the old credential once its other users no longer need it. Custom credential
IDs must be unique; a revoked ID cannot be reused.

Choose **Unassign variable** to remove a binding while retaining its stored
credential. Removing a binding or revoking a grant affects future resolution;
it cannot remove a value already held by a running process or admitted job.
It also does not erase files or other copies the program created.

Closing an environment leaves the universe's stored credentials in place.
The effects on the machine depend on its source; see
[Power and cleanup](power-and-cleanup.md).

## Use the CLI or API

With the [CLI connection settings](../using-lightspeed/sessions-and-runs.md#continue-from-the-cli)
configured, bind a stored grant by reference:

```bash
target/debug/lightspeed environment credentials bind "<environment-id>" \
  --env-name ACORN_RELEASE_TOKEN --grant "<credential-id>"
target/debug/lightspeed environment credentials list "<environment-id>"
```

Remove that assignment with:

```bash
target/debug/lightspeed environment credentials unbind "<environment-id>" \
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
record of a model API key provider, commonly `model:openai`, rather than the
session's model route ID `openai`. The CLI uses `--provider-id` for that
source. Supply exactly one of `--grant` and `--provider-id`. Exact types and
list/unbind methods are in the
[API reference](../../../crates/api/contract/api-reference.md).

A credential made from the CLI is brokered unless it is created with
`--retrievable`, on `lightspeed credential put`, `lightspeed credential
login` and `lightspeed credential github installation grant`:

```bash
target/debug/lightspeed credential put acorn-release-token \
  --token-env ACORN_RELEASE_TOKEN --retrievable
```

Declaring an existing credential again with a different exposure is refused;
declare it under a new ID. In a
[configuration document](../deployment/configuration-as-code.md) the same
setting is `exposure: retrievable` on the credential, and `diff` and `apply`
refuse a document that binds a brokered credential into an environment.

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
| Credential resolution fails before execution | Check that the referenced source is active, unexpired, and suitable for the requested audience. A message that the source "may not be bound into an environment" means the credential is brokered or the provider is not a model API key provider: bind a retrievable credential under that variable name. |
| The credential is missing from the source picker | It is brokered. Add a retrievable credential with the same value and assign that one. |
| A profile change did not rotate the machine's token | Profiles do not manage credentials. Change the environment binding directly. |
| A queued job uses an old token | Its values were resolved at admission. Submit new work after updating the binding. |
| An imported subscription stopped working | Reimport a current credential and update its binding; the static imported record is not refreshed by core. |
| Unassigning did not remove an on-disk login | A program or bootstrap wrote a separate copy. Manage that file and any running processes explicitly. |
