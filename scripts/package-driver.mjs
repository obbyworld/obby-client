// Put the compiled driver into the package wasm-pack generated.
//
// wasm-pack writes package.json itself and knows nothing about the driver, so the entry point and
// the file list are added back here, after every build.

import { readFileSync, writeFileSync } from "node:fs";

const path = "bindings/obby-wasm/pkg/package.json";
const pkg = JSON.parse(readFileSync(path, "utf8"));

for (const file of ["driver.js", "driver.d.ts"]) {
  if (!pkg.files.includes(file)) pkg.files.push(file);
}

pkg.exports = {
  ".": { types: "./obby_wasm.d.ts", default: "./obby_wasm.js" },
  "./driver": { types: "./driver.d.ts", default: "./driver.js" },
};

writeFileSync(path, `${JSON.stringify(pkg, null, 2)}\n`);
