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
  build: {
    // codemirror vendor chunk ~590KB 是编辑器内核的固有体积:内容稳定、Tauri 本地
    // 资源无网络成本,不构成问题。阈值按此标定,让真正的业务 chunk 膨胀仍能触警。
    chunkSizeWarningLimit: 650,
    rollupOptions: {
      output: {
        // CodeMirror 全家桶单独成 chunk:与业务代码分开,避免任何一行业务改动
        // 都让用户重新解析 ~400KB 的编辑器内核(也消除 500KB chunk 警告)。
        manualChunks(id: string) {
          if (/node_modules\/(@codemirror|@lezer|codemirror)\//.test(id)) {
            return "codemirror";
          }
        },
      },
    },
  },
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
