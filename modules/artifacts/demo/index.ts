import { Hono } from "hono";
import { findWorkspaceEntry, modulePermissions, WorkspaceTransferError } from "@lightspeed-ai/platform-shared";
import {
  badRequest, conflict, demoUniverseActions, notFound, readBody, universeFor,
  type DemoStore, type UniverseState,
} from "@lightspeed-ai/platform-web/demo";
import { ARTIFACT_MAX_BYTES, artifactListQuerySchema, artifactPublishSchema, type Artifact, type ArtifactContent } from "../contract";
import { artifacts } from "../module";

interface Publication {
  artifact: Artifact;
  content: ArtifactContent;
}

// Reassembling a router for the same demo retains its module data. Resetting the demo creates a fresh store.
const databases = new WeakMap<DemoStore, Map<string, Map<string, Publication>>>();

function publications(store: DemoStore, universe: UniverseState) {
  let database = databases.get(store);
  if (!database) {
    database = new Map();
    databases.set(store, database);
  }
  const scope = JSON.stringify([universe.universe.organizationId, universe.universe.id]);
  let rows = database.get(scope);
  if (!rows) {
    rows = new Map<string, Publication>();
    database.set(scope, rows);
  }
  return rows;
}

export default function artifactRoutes(store: DemoStore): Hono {
  const app = new Hono();
  app.onError((error, c) => {
    if (error instanceof WorkspaceTransferError) return c.json({ error: error.message }, error.status);
    throw error;
  });
  app.get("/", c => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    const query = artifactListQuerySchema.safeParse(c.req.query());
    if (!query.success) return badRequest(c, "Invalid artifact list filter.");
    const { workspaceId } = query.data;
    const held = modulePermissions(artifacts, demoUniverseActions(store, universe));
    const rows = Array.from(publications(store, universe).values(), row => row.artifact)
      .reverse().filter(artifact => workspaceId === undefined || artifact.workspaceId === workspaceId);
    return c.json({ artifacts: rows, permissions: { publish: held.has("publish"), retract: held.has("retract") } });
  });

  app.post("/", async c => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    const parsed = artifactPublishSchema.safeParse(await readBody(c));
    if (!parsed.success) return badRequest(c, "Invalid artifact publication.");
    const { title, workspaceId, path, expectedRevision } = parsed.data;
    const source = universe.workspaces.get(workspaceId);
    if (!source) return notFound(c, "Workspace not found.");
    if (source.row.revision !== expectedRevision) return conflict(c, "Workspace changed. Review the current file and publish again.");
    const file = findWorkspaceEntry(source.manifest, path);
    if (file.kind !== "file") return badRequest(c, "Choose a file to publish.");
    const bytes = store.blobs.get(file.blob_ref);
    if (!bytes) return notFound(c, "File content is unavailable.");
    if (file.size_bytes > ARTIFACT_MAX_BYTES || bytes.bytes > ARTIFACT_MAX_BYTES) return c.json({ error: "Artifacts must be 1 MiB or smaller." }, 413);
    const artifact: Artifact = {
      id: crypto.randomUUID(), title, workspaceId, path, blobRef: file.blob_ref,
      sizeBytes: bytes.bytes, mediaType: file.media_type ?? null,
      createdAt: new Date().toISOString(), createdBy: store.currentUser.id,
    };
    const content = { bytesBase64: bytes.bytesBase64, bytes: bytes.bytes, mediaType: artifact.mediaType };
    publications(store, universe).set(artifact.id, { artifact, content });
    return c.json({ artifact }, 201);
  });

  app.get("/:artifactId", c => {
    const universe = universeFor(store, c);
    const record = universe && publications(store, universe).get(c.req.param("artifactId"));
    return record ? c.json({ artifact: record.artifact }) : notFound(c, "Artifact not found.");
  });
  app.get("/:artifactId/content", c => {
    const universe = universeFor(store, c);
    const record = universe && publications(store, universe).get(c.req.param("artifactId"));
    return record ? c.json(record.content) : notFound(c, "Artifact not found.");
  });
  app.delete("/:artifactId", c => {
    const universe = universeFor(store, c);
    const removed = universe && publications(store, universe).delete(c.req.param("artifactId"));
    return removed ? c.body(null, 204) : notFound(c, "Artifact not found.");
  });
  return app;
}
