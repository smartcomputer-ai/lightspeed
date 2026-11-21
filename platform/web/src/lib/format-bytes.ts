export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`;
  const mib = bytes / (1024 * 1024);
  return `${mib < 10 ? mib.toFixed(1) : Math.round(mib)} MB`;
}
