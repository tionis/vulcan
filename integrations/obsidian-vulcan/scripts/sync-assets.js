"use strict";

const fs = require("node:fs");
const path = require("node:path");

const source = path.resolve(__dirname, "..");
const destination = path.resolve(source, "../../vulcan-app/assets/obsidian-vulcan");
fs.mkdirSync(destination, { recursive: true });
for (const name of ["manifest.json", "main.js", "styles.css"]) {
  fs.copyFileSync(path.join(source, name), path.join(destination, name));
}
