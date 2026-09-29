/// Reads a blob as base64 for a JSON upload body; aborts with the signal.
export function blobBase64(blob: Blob, signal: AbortSignal): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    const abort = () => reader.abort();
    reader.onload = () => resolve(String(reader.result).split(",")[1]!);
    reader.onerror = () => reject(new Error("Could not read the file."));
    reader.onabort = () => reject(new DOMException("Cancelled", "AbortError"));
    reader.onloadend = () => signal.removeEventListener("abort", abort);
    signal.throwIfAborted();
    signal.addEventListener("abort", abort, { once: true });
    reader.readAsDataURL(blob);
  });
}
