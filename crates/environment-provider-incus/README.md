# Incus environment provider

`lightspeed-provider-incus` is a stateless P118 controller, passive P119 data
endpoint, and optional P121 application edge. It depends on the public host
protocol crate, not Lightspeed's database, API implementation, engine, or
Temporal runtime.

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

Optional P121 ingress uses one provider-approved guest port per template. Set
`ingress.publicBaseUrl` to the wildcard HTTPS base and `ingress.listen` to the
provider edge listener behind deployment-managed wildcard TLS. For a template,
`publicIngress: true` requires `ingressPort`. Lightspeed callers can only
enable or disable the resulting endpoint; they cannot select a port, private
address, hostname, or proxy configuration.

The stateless edge resolves each request hostname from current Incus target
metadata and proxies HTTP, streaming bodies, and WebSockets to the approved
private guest port. The provider records the realized hostname and port in
`user.lightspeed.*` Incus target metadata so route operations remain
ownership-fenced and reconstructible without a provider database.
