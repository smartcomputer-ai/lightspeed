// Called by live_platform_connections.py against dedicated real CLI credentials.
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { join } from "node:path";
import { randomUUID } from "node:crypto";
import { createClientFromConnection } from "../../typescript/dist/node.js";
import { LightspeedApiError } from "../../typescript/dist/index.js";

const [configDir, name, mode, blobRef] = process.argv.slice(2);
const options = { configDir, name };
const denied = status => error => error instanceof LightspeedApiError && error.status === status;
if (mode === "revoked") {
  await assert.rejects(createClientFromConnection(options), denied(401));
  console.log(JSON.stringify({ revoked: true }));
} else {
  assert.equal(mode, "allowed");
  const client = await createClientFromConnection(options);
  const info = await client.initialize();
  assert.equal(info.runtime, "platform");
  const organizations = await client.platform.listOrganizations();
  assert(organizations.organizations.some(organization => organization.id === client.organizationId));
  assert(Array.isArray((await client.call("profiles/list", {})).profiles));
  const binary = Uint8Array.from({ length: 1_048_576 }, (_, index) => index % 256);
  const download = await client.openDownload("blobs/read", { blobRef });
  assert.deepEqual(new Uint8Array(await download.arrayBuffer()), binary);
  const stream = client.watch({}, { reconnect: false, signal: AbortSignal.timeout(15_000) });
  try {
    const first = await stream.next();
    assert(!first.done, "watch must deliver its initial reset before the timeout");
    assert.equal(first.value.method, "watch/reset");
  } finally {
    await stream.return();
  }
  if (info.platform.credential.kind === "personalKey") {
    assert.equal(info.platform.restrictions.readOnly, true);
    await assert.rejects(client.call("profiles/delete", { profileId: `absent-${randomUUID()}` }), denied(403));
    await assert.rejects(client.platform.listPersonalApiKeys(), denied(403));
    await assert.rejects(client.uploadBlob(blobRef, binary), denied(403));
    const other = (await readFile(join(configDir, "other-universe.txt"), "utf8")).trim();
    const outside = await createClientFromConnection({ ...options, universe: other });
    await assert.rejects(outside.call("profiles/list", {}), denied(404));
  } else {
    assert.equal((await client.uploadBlob(blobRef, binary)).blobRef, blobRef);
    const other = (await readFile(join(configDir, "other-universe.txt"), "utf8")).trim();
    const outside = await createClientFromConnection({ ...options, universe: other });
    assert(Array.isArray((await outside.call("profiles/list", {})).profiles));
    await assert.rejects(outside.openDownload("blobs/read", { blobRef }), denied(404));
  }
  console.log(JSON.stringify({ principalId: info.platform.principal.id, organizationId: client.organizationId }));
}
