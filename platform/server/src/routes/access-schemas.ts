import { z } from "zod";

export const resourceSchema = z
  .object({
    kind: z.enum(["session", "bot", "profile", "collection"]),
    id: z.string().min(1),
  })
  .strict();
export const grantSchema = z
  .object({
    subject: z
      .object({ kind: z.enum(["principal", "group"]), id: z.string().uuid() })
      .strict(),
    permission: z.enum(["read", "write"]),
  })
  .strict();
export const accessInputSchema = z
  .object({
    visibility: z.enum(["universe", "restricted"]).optional(),
    grants: z.array(grantSchema).optional(),
    root: z
      .object({ kind: z.literal("collection"), id: z.string().min(1) })
      .strict()
      .optional(),
  })
  .strict();
export const executionInputSchema = z
  .object({ kind: z.enum(["service", "personal"]) })
  .strict();
