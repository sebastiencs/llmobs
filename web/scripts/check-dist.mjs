import { readdir, readFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const distDir = fileURLToPath(new URL("../dist/", import.meta.url));
const problems = [];
const needles = ["ada@example.com", "fixtures/api", "setupWorker", "mulberry32"];

let files = [];
try {
  files = await filesUnder(distDir);
} catch {
  problems.push("dist/ is missing");
}

const indexPath = path.join(distDir, "index.html");
if (!files.includes(indexPath)) {
  problems.push("dist/index.html is missing");
} else {
  const html = await readFile(indexPath, "utf8");
  if (/https?:\/\//.test(html)) {
    problems.push("dist/index.html references an http:// or https:// URL");
  }
}

for (const file of files) {
  const relative = path.relative(distDir, file);
  if (path.basename(file) === "mockServiceWorker.js") {
    problems.push(`${relative} is named mockServiceWorker.js`);
  }
  const text = (await readFile(file)).toString("utf8");
  for (const needle of needles) {
    if (text.includes(needle)) {
      problems.push(`${relative} contains ${needle}`);
    }
  }
}

if (problems.length > 0) {
  for (const problem of problems) {
    console.error(problem);
  }
  process.exit(1);
}

async function filesUnder(dir) {
  const entries = await readdir(dir, { withFileTypes: true });
  const files = [];
  for (const entry of entries) {
    const entryPath = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      files.push(...(await filesUnder(entryPath)));
    } else if (entry.isFile()) {
      files.push(entryPath);
    }
  }
  return files;
}
