import { AsyncLocalStorage } from "node:async_hooks";
import type { DBAdapter } from "@better-auth/core/db/adapter";

export interface OrganizationKeyAuthority { userId: string; organizationId: string }

/** Better Auth checks concrete organization membership even for server key creation.
 * Platform also authorizes declared Organization Admins and deployment administrators.
 * Translate that reviewed decision for this exact real user/organization only during the
 * plugin call. This writes no membership and never creates a user or a session. */
export function organizationKeyAuthority() {
  const active = new AsyncLocalStorage<OrganizationKeyAuthority>();
  return {
    run<T>(authority: OrganizationKeyAuthority, action: () => Promise<T>): Promise<T> {
      return active.run(authority, action);
    },
    adapter(adapter: DBAdapter): DBAdapter {
      return {
        ...adapter,
        findOne: async <T>(query: Parameters<DBAdapter["findOne"]>[0]): Promise<T | null> => {
          const authority = active.getStore();
          if (authority && query.model === "member" && !query.select && !query.join && query.where.length === 2 &&
              query.where.every(clause => (!clause.operator || clause.operator === "eq") && (!clause.connector || clause.connector === "AND")) &&
              query.where.some(clause => clause.field === "userId" && clause.value === authority.userId) &&
              query.where.some(clause => clause.field === "organizationId" && clause.value === authority.organizationId)) {
            return { id: `effective:${authority.organizationId}:${authority.userId}`, userId: authority.userId,
              organizationId: authority.organizationId, role: "admin", createdAt: new Date() } as T;
          }
          return adapter.findOne<T>(query);
        },
      };
    },
  };
}
