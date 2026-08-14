const command = process.argv[2] ?? "all";
const roles = command === "all" ? ["workflows", "activities"] : [command];
for (const role of roles) {
  if (role !== "workflows" && role !== "activities") {
    throw new TypeError(`unknown foundry role: ${role}`);
  }
}

let stopping = false;
const onSignal = () => {
  stopping = true;
};
process.once("SIGINT", onSignal);
process.once("SIGTERM", onSignal);

console.log(`foundry: starting ${roles.join(", ")} in one process`);
const running = roles.map((role) =>
  role === "workflows" ? import("./workflow-worker.js") : import("./activity-worker.js"),
);

try {
  if (running.length === 1) {
    await running[0];
  } else {
    const stopped = await Promise.race(
      running.map((promise, index) => promise.then(() => roles[index])),
    );
    if (!stopping) {
      throw new Error(`foundry ${stopped} role stopped while other roles were running`);
    }
    await Promise.all(running);
  }
} finally {
  process.off("SIGINT", onSignal);
  process.off("SIGTERM", onSignal);
}
