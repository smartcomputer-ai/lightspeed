import { dev } from 'astro';

// The supervisor owns this process. Use the API so agent detection in Astro's
// CLI cannot detach the server and leave it running after ./dev.sh stop.
const server = await dev({
  root: new URL('../', import.meta.url),
  server: { host: '127.0.0.1', port: 4321 },
  vite: { server: { strictPort: true } },
});

let stopping = false;
async function stop() {
  if (stopping) return;
  stopping = true;
  await server.stop();
  process.exit(0);
}
process.once('SIGINT', stop);
process.once('SIGTERM', stop);
