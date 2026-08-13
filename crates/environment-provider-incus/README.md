# Incus environment provider

`lightspeed-provider-incus` is a stateless P118 controller and passive P119
data endpoint. It depends on the public host protocol crate, not Lightspeed's
database, API implementation, engine, or Temporal runtime.

Start from `config.example.json`. Application-level authentication for both
the provider endpoint and private envd endpoint is intentionally deferred.
Protect those listeners with the deployment network/transport boundary for
now. The Incus client certificate and key remain deployment files mounted into
the provider process.

There is intentionally no provider ID in this configuration. The operator
assigns that identity when registering the reachable controller endpoint.

Build an image with `image/build-image.sh`, then replace the example template's
image fingerprint with the immutable fingerprint. The provider deliberately
rejects aliases and arbitrary universe-supplied images or cloud-init.

```bash
cargo run -p environment-provider-incus -- \
  --config crates/environment-provider-incus/config.example.json
```

Register its controller as `ws://127.0.0.1:19090/control`. Lightspeed derives
the sibling `/routes/...` endpoint and connects only while an environment is
being used; the provider then dials private envd and reaps idle connections.
Bindings are
configured twice for different purposes: Lightspeed stores only admission and
routing identity, while this file is the provider's authoritative entitlement,
quota, template, and physical-network policy.
