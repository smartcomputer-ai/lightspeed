import { z } from "zod";

export const organizationModuleSchema = z.object({
  id: z.string(),
  name: z.string(),
  description: z.string(),
  enabled: z.boolean(),
});

/** A distribution selects module definitions. Organizations override their defaults. */
export interface PlatformModule {
  id: string;
  name: string;
  description: string;
  defaultEnabled: boolean;
}

export function organizationModuleView(module: PlatformModule, enabled = module.defaultEnabled): z.infer<typeof organizationModuleSchema> {
  return { id: module.id, name: module.name, description: module.description, enabled };
}
