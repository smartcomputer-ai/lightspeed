export interface ChannelConnectorStatus {
  url: string;
  reachable: boolean;
  httpStatus: number | null;
  health?: unknown;
  error?: string;
}

export async function readChannelsStatus(
  urls: readonly string[],
  options: { fetch?: typeof fetch; timeoutMs?: number } = {},
): Promise<ChannelConnectorStatus[]> {
  const request = options.fetch ?? fetch;
  const timeoutMs = options.timeoutMs ?? 2_000;
  return Promise.all(
    urls.map(async (url): Promise<ChannelConnectorStatus> => {
      try {
        const response = await request(new URL("/healthz", ensureDirectoryUrl(url)), {
          signal: AbortSignal.timeout(timeoutMs),
        });
        const health: unknown = await response.json();
        return {
          url,
          reachable: true,
          httpStatus: response.status,
          health,
        };
      } catch (error) {
        return {
          url,
          reachable: false,
          httpStatus: null,
          error: errorMessage(error),
        };
      }
    }),
  );
}

function ensureDirectoryUrl(value: string): URL {
  return new URL(value.endsWith("/") ? value : `${value}/`);
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
