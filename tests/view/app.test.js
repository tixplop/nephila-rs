"use strict";
const test = require("node:test");
const assert = require("node:assert");
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");

const dir = path.join(__dirname, "..", "..", "src", "view");

test("REQ025_app_js_se_compile", () => {
  const src = fs.readFileSync(path.join(dir, "app.js"), "utf8");
  assert.doesNotThrow(() => new vm.Script(src, { filename: "app.js" }));
});

test("REQ025_app_js_sans_ressource_externe", () => {
  const src = fs.readFileSync(path.join(dir, "app.js"), "utf8");
  for (const bad of ["http://", "https://", "fetch(", "import(", "XMLHttpRequest", "innerHTML"]) {
    assert.ok(!src.includes(bad), bad);
  }
});

test("REQ039_app_js_utilise_le_modele", () => {
  const src = fs.readFileSync(path.join(dir, "app.js"), "utf8");
  for (const name of ["allView", "layoutStep", "linksShown", "onWheel", "onPanStart", "onPanEnd"]) {
    assert.ok(src.includes(name + "("), name);
  }
  assert.ok(!src.includes("d2 > 90000"), "double boucle de répulsion");
});
