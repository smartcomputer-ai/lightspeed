import { validWorkspaceTransferPath } from "@lightspeed-ai/platform-shared";
import { z } from "zod";

export const ARTIFACT_MAX_BYTES = 1024 * 1024;

const workspaceId = z.string().min(1).max(256).refine(value => !value.includes("\0"), "Invalid workspace ID");
export const artifactListQuerySchema = z.object({ workspaceId: workspaceId.optional() });

export const artifactPublishSchema = z.strictObject({
  workspaceId,
  path: z.string().max(4096).refine(validWorkspaceTransferPath, "Invalid relative file path"),
  title: z.string().trim().min(1).max(200).refine(value => !value.includes("\0"), "Invalid title"),
  expectedRevision: z.number().int().nonnegative(),
});

export interface Artifact {
  id: string;
  title: string;
  workspaceId: string;
  path: string;
  blobRef: string;
  sizeBytes: number;
  mediaType: string | null;
  createdBy: string;
  createdAt: string;
}

export interface ArtifactList {
  artifacts: Artifact[];
  permissions: { publish: boolean; retract: boolean };
}

export interface ArtifactContent {
  bytes: number;
  bytesBase64: string;
  mediaType: string | null;
}
