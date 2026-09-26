export class DevError extends Error {
  constructor(message, { details = [], hint, cause } = {}) {
    super(message, { cause });
    this.name = "DevError";
    this.details = details;
    this.hint = hint;
  }
}

export function connectionProblem(error) {
  const cause = error?.cause ?? error;
  const code = cause?.code ?? cause?.errors?.[0]?.code;
  if (code === "ECONNREFUSED") return "Connection refused: nothing accepted the connection.";
  if (code === "ENOTFOUND") return "Hostname could not be resolved.";
  if (code === "EACCES" || code === "EPERM") return "Permission denied while opening the connection.";
  if (code === "ETIMEDOUT" || error?.name === "TimeoutError") return "Connection timed out.";
  return `${code ? `${code}: ` : ""}${cause?.message ?? String(error)}`;
}

// Diagnostics can contain database URLs. Never repeat their embedded password.
export function redact(text) {
  return String(text)
    .replace(/(\b[a-z][a-z\d+.-]*:\/\/)[^\s/@]+:[^\s/@]*@/gi, "$1[redacted]@")
    .replace(/((?:token|password|secret|api[_-]?key)=)[^\s&]+/gi, "$1[redacted]");
}

export function formatFailure(error, { phase = "running the launcher", debug = false } = {}) {
  const lines = [`\ndev.sh: Failed while ${phase}.`, `\n${error.message ?? error}`];
  if (error.details?.length) lines.push("", ...error.details);
  const fallbackHint = phase === "reading command arguments"
    ? "Run ./dev.sh --help for supported commands and options."
    : "Review the error above and check .env and exported settings. Run ./dev.sh --help for supported commands.";
  lines.push("", `Next: ${error.hint ?? fallbackHint}`);
  if (debug) lines.push("", error.stack ?? String(error), ...(error.cause ? [`Caused by: ${error.cause.stack ?? error.cause}`] : []));
  else lines.push("For launcher stack traces, repeat the command with --debug.");
  return redact(lines.join("\n"));
}
