The core is sound and smaller than the diff suggests. I have three concerns: the Contributor/Operator boundary leaks, there are a handful of small holes and one regression, and the enforcement and audit layering is where nearly all the complexity and cost sits.

- **What's solid:**
  - The `access` crate is 542 lines and clear.
  - Identity writes authorize, commit and audit in one transaction.
  - Assertions never union privileges.
  - The asserted user always comes from the login session.
  - All 113 service handlers authorize.
  - Ownership always comes from storage, never from request parameters.
- **How this was checked:**
  - I read the model, store, migrations, gateway authentication, audit and ownership code myself.
  - Four parallel reviewers covered the rest.
  - I re-verified every item below in the code unless it is marked "per the reviewers".

## A. Security: worth fixing now

1. **Contributors can reach Operator powers and stored secrets through ordinary "use" paths.** The Contributor role is new, so this matters now.
   - **Bot poll trigger:** any Contributor can create a bot and attach any retrievable credential to a poll trigger with any http(s) URL. The token is then sent to that URL.
     - Channel account tokens are retrievable.
     - Admission only checks that the credential is active and retrievable ([bots_api.rs:163](crates/temporal-server/src/gateway/service/bots_api.rs#L163)).
     - The URL check is scheme-only ([validate.rs:257](crates/bots/src/validate.rs#L257)), and there is no private-network guard.
   - **Configurator setup:** its service principal gets the Operator role, and its key backs a shared MCP server with `approval: "never"` plus a shared profile ([setups.ts:259](platform/server/src/routes/setups.ts#L259)). Any Contributor's session can therefore drive Operator actions, and they are attributed to "Configurator".
   - **Injected credentials:** per the reviewers, credentials injected into an environment are readable by any Contributor's job. That is inherent until per-resource restrictions exist.
   - **Fix:**
     - Require `ConfigureResource` to attach a credential to a trigger, or bind the URL to the credential's audience.
     - Gate the Configurator to Operator and above.
     - Otherwise, say in the doc that a Contributor is trusted with universe secrets in this slice.
2. **Auth mode defaults to `single`** when `LIGHTSPEED_AUTH_MODE` is unset ([config.rs:61](crates/temporal-server/src/config.rs#L61)). The result is an unauthenticated DeploymentAdmin. Require the variable explicitly and have `./dev.sh` set it.
3. **GitHub login has two problems.**
   - **Implicit linking by email:** the auth library links accounts by verified email by default, and admin-created and bootstrap users are inserted with `emailVerified: true` ([identity.ts:59](platform/server/src/routes/identity.ts#L59)). This contradicts "never matches identities by email".
   - **First-time sign-up cannot work:**
     - `corePrincipalId {required, input: false}` ([auth.ts:10](platform/server/src/auth.ts#L10)) makes better-auth throw before the create hook runs.
     - The live test calls `internalAdapter.createUser` directly, so it misses this.
     - The create hook is therefore dead in production paths.
   - **Fix:** at minimum set `accountLinking: { enabled: false }`.
4. **HMAC-verified webhooks are broken.**
   - [hooks.rs:98](crates/temporal-server/src/bots/hooks.rs#L98) still calls the public `lease_auth_grant`.
   - That method now needs a request context and the `LeaseCredentials` capability, and webhook ingest has neither.
   - Each delivery returns 503 and writes a denial row.
   - No test covers `HmacSha256` verification.
5. **Audit amplifies writes and records noise.**
   - Unauthenticated failures each insert a row ([authentication.rs:60](crates/temporal-server/src/gateway/authentication.rs#L60)).
   - There is no rate limit, and the database pool defaults to 10 connections.
   - Every `Rejected` response is stored as a denial ([http.rs:1085](crates/temporal-server/src/gateway/http.rs#L1085)). `Rejected` is the generic business-rule kind with about 120 call sites, such as "session is not open".
   - **Fix:** add distinct `unauthenticated` and `forbidden` error kinds, and audit only denials that happen after authentication.
6. **A DeploymentAdmin can mint a key bound to another human user.**
   - `may_issue_key` puts no kind or self constraint on the target ([lib.rs:504](crates/access/src/lib.rs#L504)).
   - The admin then acts as that user, with that user's attribution.
   - Restrict non-self minting to service principals.
7. **Fail-closed audit blocks stop actions and can lose a result.**
   - `runs/cancel` and `session/close` refuse when the audit insert fails.
   - A failed completion-row write discards an already-committed result ([deployment.rs:113](crates/temporal-server/src/gateway/deployment.rs#L113)). For `api-keys/create` that loses the secret.
   - Make the completion row and the stop actions best-effort.
8. **Smaller items.**
   - **Caller-supplied workflow ID:** `session/managed/start` is classified `CreateSession` but takes `receiver.workflowId` verbatim, with no universe-prefix check. This is pre-existing, but Contributors can now reach it.
   - **Directory scope:** `identity/directory` returns every principal and group in the deployment to any universe Admin.
   - **Error leak:** Platform's new `onError` returns raw error messages. For Drizzle errors that includes the SQL and its parameters.
   - **Key prefix in audit:** key revocation copies a caller-supplied `keyPrefix` into the audit row before validating it.


## C. Decide later, not urgent

- **Ownership reservations are never released.**
  - A deleted ID is unusable by anyone but its creator, Admins included.
  - IDs can be squatted.
  - Nobody can delete an offboarded user's sessions.
- **`ManageIdentity` amounts to DeploymentAdmin.** That is fine, but the doc should say so.
- **Handler enforcement is by convention only.** Turn the reviewers' "every handler authorizes" script into a test.

Fine to leave for a later slice: audit retention and export, general rate limiting, private sessions, and effect-time revocation.

I can turn sections A and B into a prioritized fix list in the roadmap doc if you want to hand this to the implementer.