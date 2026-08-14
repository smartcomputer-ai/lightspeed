const CTRL_C = "\u0003";
const BACKSPACE = "\u007f";

/// Hidden-input prompt for passwords (raw mode, echoes nothing).
export async function promptHidden(label: string): Promise<string> {
  process.stderr.write(label);
  const stdin = process.stdin;
  stdin.setRawMode?.(true);
  stdin.resume();
  return new Promise((resolve, reject) => {
    let value = "";
    const onData = (chunk: Buffer) => {
      for (const char of chunk.toString("utf8")) {
        if (char === "\r" || char === "\n") {
          stdin.setRawMode?.(false);
          stdin.pause();
          stdin.off("data", onData);
          process.stderr.write("\n");
          resolve(value);
          return;
        }
        if (char === CTRL_C) {
          stdin.setRawMode?.(false);
          stdin.pause();
          reject(new Error("aborted"));
          return;
        }
        if (char === BACKSPACE || char === "\b") {
          value = value.slice(0, -1);
        } else {
          value += char;
        }
      }
    };
    stdin.on("data", onData);
  });
}
