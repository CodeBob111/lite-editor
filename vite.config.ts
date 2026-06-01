import { defineConfig, type Plugin } from "vite";

const host = process.env.TAURI_DEV_HOST;

function fullReloadPlugin(): Plugin {
  return {
    name: "full-reload-on-change",
    handleHotUpdate({ server }) {
      server.ws.send({ type: "full-reload" });
      return [];
    },
  };
}

export default defineConfig(async () => ({
  clearScreen: false,
  plugins: [fullReloadPlugin()],
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      ignored: ["**/src-tauri/**"],
    },
  },
}));
