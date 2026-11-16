# Run inspector

A small page for looking through sessions and runs and acting on them: the session
list, each run's messages and tool calls, full tool output, approvals, and cancel.
It shows the shape to copy for your own UI, not a finished product.

The browser never holds the runtime token. It uses an ordinary `LightspeedClient` pointed
at [`server.ts`](server.ts), which checks the caller and forwards only the listed
operations using generated `HTTP_OPERATIONS` resource bindings. [`app.ts`](app.ts) is the browser side.

## Run it

From the repository root, with Node.js 24 and a built SDK:

```bash
npm run build --workspace @lightspeed-ai/sdk

LIGHTSPEED_API_URL=http://127.0.0.1:18080/api/v1 \
LIGHTSPEED_RUNTIME_TOKEN=lsr_... \
LIGHTSPEED_UNIVERSE=your-universe \
INSPECTOR_PASSWORD=choose-one \
node clients/typescript/examples/run-inspector/server.ts
```

Open <http://127.0.0.1:18090> and sign in with any username and that password. Set
`PORT` to change the port.

The configured runtime token grants access to the runtime. This server limits the
forwarded operations to the selected universe. Anyone signed in can read every
session there, cancel runs, and decide approvals.

## What to change for a real app

- **Sign-in:** replace `signedIn` in `server.ts` with your own login. The HTTP Basic
  check here exists only to keep the example free of dependencies.
- **Access:** authorize the caller and URL target in `forward` before forwarding. The
  example allowlist grants every signed-in user the same universe access.
- **Attribution:** set `x-lightspeed-actor` from your authenticated user's ID on
  upstream requests. Runtime tokens may assert an actor; attribution grants no permissions.
- **Serving and framework:** this has no build step, so the server strips types from
  `app.ts` and serves the SDK's files directly. Use your own bundler and framework.
