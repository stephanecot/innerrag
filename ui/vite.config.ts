import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// In development the UI talks to a server started on :18080 (see README).
const target = process.env.INNERRAG_DEV_SERVER ?? "http://localhost:18080";

export default defineConfig({
  plugins: [react()],
  server: { proxy: { "/api": target, "/mcp": target } },
  build: { chunkSizeWarningLimit: 900 },
});
