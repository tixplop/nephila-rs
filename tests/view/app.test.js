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

test("REQ150_app_js_utilise_le_modele_pour_les_communautes", () => {
  const src = fs.readFileSync(path.join(dir, "app.js"), "utf8");
  for (const name of ["nodeColor(", "communityLegend(", "colorChoice(", "color-community", "color-note", "legend"]) {
    assert.ok(src.includes(name), name);
  }
  for (const hex of ["#36b", "#c60", "#888"]) assert.ok(!src.includes(hex), hex + " en dur dans app.js");
});

test("REQ150_AC8 AC-150-8 app.js : description par textContent, champ et span de la légende", () => {
  const src = fs.readFileSync(path.join(dir, "app.js"), "utf8");
  assert.ok(src.includes('"Description de la communauté"'));
  assert.ok(src.indexOf('"Communauté"') < src.indexOf('"Description de la communauté"'));
  assert.ok(src.indexOf('"Description de la communauté"') < src.indexOf('"Résumé"'));
  assert.ok(src.includes('el("span", entry.description, "description")'));
  assert.ok(src.includes('entry.text + " : " + entry.description'));
  assert.ok(!src.includes("innerHTML"));
  const css = fs.readFileSync(path.join(dir, "style.css"), "utf8");
  assert.ok(css.includes("#legend .description"));
});
