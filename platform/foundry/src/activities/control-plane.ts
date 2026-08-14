import { schema, type Db } from "@lightspeed/platform-db";
import type { FoundryReleaseRecordArgs } from "../contracts/foundry.js";

export interface RecordReleaseInput {
  packId: string;
  invocationId: string;
  release: FoundryReleaseRecordArgs;
}

export interface FoundryControlPlaneActivities {
  recordRelease(input: RecordReleaseInput): Promise<void>;
}

export function createFoundryControlPlaneActivities(db: Db): FoundryControlPlaneActivities {
  return {
    async recordRelease({ packId, invocationId, release }) {
      await db
        .insert(schema.foundryReleases)
        .values({
          packId,
          invocationId,
          sourceCommit: release.sourceCommit,
          artifactDigest: release.artifactDigest,
          target: release.target,
          outcome: release.outcome,
          initiatedBy: release.initiatedBy,
          smokePassed: release.smokePassed,
          detailsRef: release.detailsRef,
        })
        .onConflictDoNothing({ target: schema.foundryReleases.invocationId });
    },
  };
}
