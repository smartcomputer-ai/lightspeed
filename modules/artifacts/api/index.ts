import { createHash } from "node:crypto";
import { Hono } from "hono";
import { bodyLimit } from "hono/body-limit";
import { z } from "zod";
import {
  asWorkspaceManifest, parseBody, type ModuleServices, type UniverseModuleEnv,
} from "@lightspeed-ai/platform-backend/modules";
import { findWorkspaceEntry } from "@lightspeed-ai/platform-shared";
import { ARTIFACT_MAX_BYTES, artifactListQuerySchema, artifactPublishSchema, type ArtifactContent, type ArtifactList } from "../contract.js";
import { getArtifact, getArtifactContent, listArtifacts, publishArtifact, retractArtifact } from "../db/index.js";

const artifactId = z.uuid();

export default function artifactRoutes({ db }: ModuleServices): Hono<UniverseModuleEnv> {
  const app = new Hono<UniverseModuleEnv>();
  app.use("*", async (c, next) => {
    c.header("Cache-Control", "private, no-store");
    await next();
  });

  app.get("/", async (c) => {
    const query = artifactListQuerySchema.safeParse(c.req.query());
    if (!query.success) return c.json({ error: "Invalid workspace filter." }, 400);
    const artifacts = await listArtifacts(db, c.get("access"), query.data.workspaceId);
    const permissions = c.get("permissions");
    return c.json({ artifacts, permissions: {
      publish: permissions.has("publish"), retract: permissions.has("retract"),
    } } satisfies ArtifactList);
  });

  app.post("/", bodyLimit({ maxSize: 32 * 1024 }), async (c) => {
    const body = await parseBody(c, artifactPublishSchema);
    if (!body.ok) return body.response;
    const { workspaceId, path, title, expectedRevision } = body.data;
    const client = c.get("client");
    const { workspace } = await client.call("vfs/workspaces/read", { workspaceId });
    if (workspace.revision !== expectedRevision) return c.json({ error: "Workspace changed. Refresh and select the file again." }, 409);

    // Follow the workspace's immutable head. The caller cannot supply a snapshot or blob digest.
    const { manifest } = await client.call("vfs/snapshots/read", { snapshotRef: workspace.headSnapshotRef });
    const entry = findWorkspaceEntry(asWorkspaceManifest(manifest), path);
    if (entry.kind !== "file") return c.json({ error: "Choose a file to publish." }, 400);
    if (entry.size_bytes > ARTIFACT_MAX_BYTES) return c.json({ error: "Artifacts can be at most 1 MiB." }, 413);
    if (!Number.isSafeInteger(entry.size_bytes) || entry.size_bytes < 0) return c.json({ error: "Invalid source file size." }, 502);

    const blob = await client.openDownload("blobs/read", { blobRef: entry.blob_ref }, { signal: c.req.raw.signal });
    const content = await readSnapshotBytes(blob, entry.size_bytes);
    if (!content || `sha256:${createHash("sha256").update(content).digest("hex")}` !== entry.blob_ref) {
      return c.json({ error: "The source bytes did not match the workspace snapshot." }, 502);
    }
    const artifact = await publishArtifact(db, c.get("access"), {
      workspaceId, path, title, blobRef: entry.blob_ref, mediaType: entry.media_type ?? null, sizeBytes: content.byteLength, content,
    });
    return c.json({ artifact }, 201);
  });

  app.get("/:artifactId", async (c) => {
    const id = c.req.param("artifactId");
    const artifact = artifactId.safeParse(id).success && await getArtifact(db, c.get("access"), id);
    return artifact ? c.json({ artifact }) : c.json({ error: "Artifact not found." }, 404);
  });

  app.get("/:artifactId/content", async (c) => {
    const id = c.req.param("artifactId");
    const artifact = artifactId.safeParse(id).success && await getArtifactContent(db, c.get("access"), id);
    if (!artifact) return c.json({ error: "Artifact not found." }, 404);
    return c.json({
      bytesBase64: Buffer.from(artifact.content).toString("base64"),
      bytes: artifact.content.byteLength,
      mediaType: artifact.mediaType,
    } satisfies ArtifactContent);
  });

  app.delete("/:artifactId", async (c) => {
    const id = c.req.param("artifactId");
    const removed = artifactId.safeParse(id).success && await retractArtifact(db, c.get("access"), id);
    return removed ? c.body(null, 204) : c.json({ error: "Artifact not found." }, 404);
  });

  return app;
}

// A caller controls the manifest. Bound the actual download even if its declared length is wrong.
async function readSnapshotBytes(response: Response, expected: number): Promise<Buffer | undefined> {
  const length = response.headers.get("content-length");
  if (length !== null && Number(length) !== expected) {
    await response.body?.cancel();
    return;
  }
  const content = Buffer.alloc(expected);
  if (!response.body) return expected === 0 ? content : undefined;
  const reader = response.body.getReader();
  let offset = 0;
  try {
    while (true) {
      const { value, done } = await reader.read();
      if (done) return offset === expected ? content : undefined;
      if (value.length > expected - offset) {
        await reader.cancel();
        return;
      }
      content.set(value, offset);
      offset += value.length;
    }
  } finally { reader.releaseLock(); }
}
