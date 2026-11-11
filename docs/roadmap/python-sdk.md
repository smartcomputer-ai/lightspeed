# Python SDK and local internals demo

[Issue #145](https://github.com/smartcomputer-ai/lightspeed/issues/145)
tracks parity with the TypeScript gateway client.

The checkout now has generated Python types and async helpers under
`clients/python`, shared TypeScript and Python parity fixtures, and a
self-contained Streamlit example under `examples/streamlit-lightspeed`.
The demo shows how a selected run moves through the local Lightspeed stack.

## Release follow-up

1. Confirm maintainer access to the reserved `lightspeed-ai` PyPI project.
2. Build a wheel and source distribution in the stable-tag workflow from the
   validated release commit. Stamp both with the Lightspeed release version,
   install the wheel in a clean environment, and record artifact checksums.
3. Enroll the workflow in PyPI Trusted Publishing using a protected GitHub
   environment and publish the verified artifacts with the next eligible
   Lightspeed release.
4. Verify a pinned install against the matching gateway and add the PyPI
   install command to the published integration guide.
