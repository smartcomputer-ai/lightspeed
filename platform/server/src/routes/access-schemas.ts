import { z } from "zod";

export const resourceSchema = z
  .object({
    kind: z.enum([
      "session",
      "bot",
      "profile",
      "workspace",
      "environment",
      "mcp_server",
    ]),
    id: z.string().min(1),
  })
  .strict();
/// Blobs are read through the sessions and bots that admitted them; a
/// workspace's files are read by path.
export const contentResourceSchema = z
  .object({ kind: z.enum(["session", "bot"]), id: z.string().min(1) })
  .strict();
export const grantSchema = z
  .object({
    subject: z
      .object({ kind: z.enum(["principal", "group"]), id: z.string().uuid() })
      .strict(),
    permission: z.enum(["read", "write", "use"]),
  })
  .strict();
export const accessInputSchema = z
  .object({
    visibility: z.enum(["universe", "restricted"]).optional(),
    grants: z.array(grantSchema).optional(),
  })
  .strict();
export const executionInputSchema = z
  .object({ kind: z.enum(["service", "personal"]) })
  .strict();
