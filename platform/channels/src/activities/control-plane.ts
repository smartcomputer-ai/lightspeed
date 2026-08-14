import { ApplicationFailure } from "@temporalio/common";
import { and, eq } from "drizzle-orm";
import { schema, type Db } from "@lightspeed/platform-db";
import type {
  AssertBindingActiveInput,
  ControlPlaneActivities,
} from "../contracts/control-plane.js";

export function createControlPlaneActivities(db: Db): ControlPlaneActivities {
  return {
    async assertBindingActive(input) {
      if (!(await bindingIsActive(db, input))) {
        throw ApplicationFailure.nonRetryable(
          `channel binding ${input.bindingId} is no longer active for this route`,
          "InactiveChannelBinding",
        );
      }
    },
  };
}

async function bindingIsActive(db: Db, input: AssertBindingActiveInput): Promise<boolean> {
  const [row] = await db
    .select({
      enabled: schema.bindings.enabled,
      matchScope: schema.bindings.matchScope,
      pairingCode: schema.bindings.pairingCode,
      universeStatus: schema.universes.status,
      accountEnabled: schema.channelAccounts.enabled,
      pairingKey: schema.pairings.key,
    })
    .from(schema.bindings)
    .innerJoin(schema.universes, eq(schema.universes.id, schema.bindings.universeId))
    .innerJoin(
      schema.channelAccounts,
      and(
        eq(schema.channelAccounts.id, schema.bindings.channelAccountId),
        eq(schema.channelAccounts.provider, input.route.provider),
        eq(schema.channelAccounts.accountId, input.route.accountId),
      ),
    )
    .leftJoin(
      schema.pairings,
      and(
        eq(schema.pairings.bindingId, schema.bindings.id),
        eq(schema.pairings.channelAccountId, schema.channelAccounts.id),
        eq(schema.pairings.chatId, input.route.chatId),
      ),
    )
    .where(eq(schema.bindings.id, input.bindingId))
    .limit(1);
  if (row === undefined) {
    return false;
  }
  return (
    row.enabled &&
    row.accountEnabled &&
    row.universeStatus === "active" &&
    (row.matchScope === null || row.matchScope === input.scope) &&
    (row.pairingCode === null || row.pairingKey !== null)
  );
}
