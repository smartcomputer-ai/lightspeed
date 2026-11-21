import { defineConfig } from "vitest/config";

export default defineConfig(({ mode }) => ({
  test: {
    include: [
      "src/**/*.test.ts",
      "test/**/*.test.ts",
      ...(mode === "host" ? [] : ["../../modules/*/{api,db}/**/*.test.ts", "../../distributions/standard/test/api/**/*.test.ts"]),
    ],
  },
}));
