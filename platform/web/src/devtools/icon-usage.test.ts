import { describe, expect, it } from "vitest";
import { collectIconUsage, iconImports } from "./icon-usage";

describe("icon usage", () => {
  it("reads icon imports, skipping types and resolving aliases", () => {
    const source = `
      import { useState } from "react";
      import {
        Check,
        Globe as GlobeIcon,
        type LucideIcon,
      } from "lucide-react";
      import type { Icon } from "@tabler/icons-react";
      import { IconX } from "@tabler/icons-react"
      import { BotIcon } from "@/components/icons/bot";
    `;
    expect(iconImports(source)).toEqual([
      { name: "Check", library: "lucide" },
      { name: "Globe", library: "lucide" },
      { name: "IconX", library: "tabler" },
      { name: "BotIcon", library: "local" },
    ]);
  });

  it("lists each icon once with its files, most used first", () => {
    const usage = collectIconUsage({
      "/src/a.tsx": `import { Check, Plus } from "lucide-react";`,
      "/src/b.tsx": `import { Check } from "lucide-react"; import { Check as Tick } from "lucide-react";`,
    });
    expect(usage).toEqual([
      { name: "Check", library: "lucide", files: ["src/a.tsx", "src/b.tsx"] },
      { name: "Plus", library: "lucide", files: ["src/a.tsx"] },
    ]);
  });
});
