"use strict";

// Bundles the extension entry (client.js) and its node dependencies
// (vscode-languageclient, etc.) into a single dist/extension.js. The `vscode`
// module is provided by the host at runtime and must stay external. Binary
// resolution (bin/<platform>/) and the Test Explorer work unchanged because the
// bundle is plain CommonJS and reads paths from `context.extensionPath` at
// runtime, not from the bundle location.

const esbuild = require("esbuild");

const watch = process.argv.includes("--watch");
const production = process.argv.includes("--production");

const options = {
  entryPoints: ["client.js"],
  bundle: true,
  outfile: "dist/extension.js",
  platform: "node",
  format: "cjs",
  target: "node18",
  // `vscode` is injected by the extension host; never bundle it.
  external: ["vscode"],
  sourcemap: !production,
  minify: production,
  logLevel: "info",
};

async function main() {
  if (watch) {
    const ctx = await esbuild.context(options);
    await ctx.watch();
    console.log("esbuild: watching for changes...");
  } else {
    await esbuild.build(options);
    console.log("esbuild: built dist/extension.js");
  }
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
