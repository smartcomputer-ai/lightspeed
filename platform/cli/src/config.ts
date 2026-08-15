import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";

export interface CliConfig {
  baseUrl: string;
  token: string | null;
}

const configDir =
  process.env.LIGHTSPEED_PLATFORM_CONFIG_DIR ??
  path.join(os.homedir(), ".config", "lightspeed-platform");
const configPath = path.join(configDir, "config.json");

export function loadConfig(): CliConfig {
  try {
    const raw = JSON.parse(readFileSync(configPath, "utf8")) as Partial<CliConfig>;
    return {
      baseUrl: raw.baseUrl ?? "http://localhost:3000",
      token: raw.token ?? null,
    };
  } catch {
    return { baseUrl: "http://localhost:3000", token: null };
  }
}

export function saveConfig(config: CliConfig): void {
  mkdirSync(configDir, { recursive: true });
  writeFileSync(configPath, `${JSON.stringify(config, null, 2)}\n`, {
    mode: 0o600,
  });
}
