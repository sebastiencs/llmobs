import { defineConfig } from "tsdown";

export default defineConfig({
  entry: ["src/index.ts", "src/anthropic.ts", "src/langchain.ts", "src/openai.ts"],
  format: "esm",
  dts: true,
  platform: "neutral",
  target: "node22",
  clean: true,
});
