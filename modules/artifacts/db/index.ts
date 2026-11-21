import { and, desc, eq } from "drizzle-orm";
import type { ModuleDatabase } from "@lightspeed-ai/platform-backend/modules";
import type { Artifact } from "../contract.js";
import { publications } from "./schema.js";

interface Scope {
  organizationId: string;
  universeId: string;
}

type Snapshot = Pick<Artifact, "title" | "workspaceId" | "path" | "blobRef" | "mediaType" | "sizeBytes"> & {
  content: Uint8Array;
};

// Never select snapshot bytes for the shelf or the publish response.
const metadata = {
  id: publications.id,
  title: publications.title,
  workspaceId: publications.workspaceId,
  path: publications.path,
  blobRef: publications.blobRef,
  sizeBytes: publications.sizeBytes,
  mediaType: publications.mediaType,
  createdBy: publications.createdBy,
  createdAt: publications.createdAt,
};

function inScope(scope: Scope) {
  return and(eq(publications.organizationId, scope.organizationId), eq(publications.universeId, scope.universeId));
}

function artifact(row: Omit<Artifact, "createdAt"> & { createdAt: Date }): Artifact {
  return { ...row, createdAt: row.createdAt.toISOString() };
}

export async function listArtifacts(db: ModuleDatabase, scope: Scope, workspaceId?: string): Promise<Artifact[]> {
  const rows = await db.select(metadata).from(publications).where(and(
    inScope(scope), workspaceId === undefined ? undefined : eq(publications.workspaceId, workspaceId),
  )).orderBy(desc(publications.createdAt), desc(publications.id));
  return rows.map(artifact);
}

export async function publishArtifact(db: ModuleDatabase, scope: Scope & { actorId: string }, snapshot: Snapshot): Promise<Artifact> {
  const [row] = await db.insert(publications).values({
    organizationId: scope.organizationId,
    universeId: scope.universeId,
    createdBy: scope.actorId,
    ...snapshot,
  }).returning(metadata);
  return artifact(row!);
}

export async function getArtifact(db: ModuleDatabase, scope: Scope, id: string): Promise<Artifact | undefined> {
  const [row] = await db.select(metadata).from(publications).where(and(inScope(scope), eq(publications.id, id)));
  return row && artifact(row);
}

export async function getArtifactContent(db: ModuleDatabase, scope: Scope, id: string) {
  const [row] = await db.select({ content: publications.content, mediaType: publications.mediaType })
    .from(publications).where(and(inScope(scope), eq(publications.id, id)));
  return row;
}

export async function retractArtifact(db: ModuleDatabase, scope: Scope, id: string): Promise<boolean> {
  const removed = await db.delete(publications).where(and(inScope(scope), eq(publications.id, id))).returning({ id: publications.id });
  return removed.length !== 0;
}
