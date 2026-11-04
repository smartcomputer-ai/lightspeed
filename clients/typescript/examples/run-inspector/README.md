# Run inspector

A small page for looking through sessions and runs and acting on them: the session
list, each run's messages and tool calls, full tool output, approvals, and cancel.
It shows the shape to copy for your own UI, not a finished product.

The browser never holds the API key. It uses an ordinary `LightspeedClient` pointed
at [`server.ts`](server.ts), which checks the caller and forwards only the listed
methods using `createRpcProxy`. [`app.ts`](app.ts) is the browser side.

## Run it

From the repository root, with Node.js 24 and a built SDK:

```bash
npm run build --workspace @lightspeed-ai/sdk

LIGHTSPEED_API_URL=http://127.0.0.1:18080/rpc \
LIGHTSPEED_API_KEY=lsk_... \
INSPECTOR_PASSWORD=choose-one \
node clients/typescript/examples/run-inspector/server.ts
```

Open <http://127.0.0.1:18090> and sign in with any username and that password. Set
`LIGHTSPEED_UNIVERSE` too if the key is a deployment key, and `PORT` to change the port.

The key needs the `session` group. This page can read every session in the key's
universe and can cancel runs and decide approvals there, so run it only where everyone
who can sign in should have that access.

## What to change for a real app

- **Sign-in:** replace `signedIn` in `server.ts` with your own login. The HTTP Basic
  check here exists only to keep the example free of dependencies.
- **Access:** decide per user in `authorize`. Everything in a universe is one readable
  namespace to a key allowed to read it, including `blobs/read`, so give an app that
  must not see some work its own universe and key.
- **Attribution:** return `{ actor: user.id }` from `authorize` so cancels and approvals
  record who made them. The key must be created with `assertActor`.
- **Serving and framework:** this has no build step, so the server strips types from
  `app.ts` and serves the SDK's files directly. Use your own bundler and framework.
