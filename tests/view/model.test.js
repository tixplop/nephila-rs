"use strict";
const test = require("node:test");
const assert = require("node:assert/strict");
const model = require("../../src/view/model.js");
const helpers = require("./helpers.js");

const ORDER = "type:com.acme.core.OrderService";

function idsOf(g, set) {
  return [...set].map((i) => g.ids[i]).sort();
}

test("chargement de la fixture maven3", () => {
  const data = helpers.loadMaven3();
  const g = model.load(data);
  assert.equal(g.n, data.nodes.length);
  assert.equal(g.m, data.edges.length);
});

test("load rejette des données invalides", () => {
  assert.throws(() => model.load({}), /invalides/);
  assert.throws(() => model.load({ nodes: [["a", 0, "a", "a"]], edges: [[0, 5, 0]], nodeKinds: ["x"], edgeKinds: ["y"] }), /hors limites/);
});

test("REQ025_AC5 AC-025-5 vue initiale : modules et liens depends_on", () => {
  const g = model.load(helpers.loadMaven3());
  const view = model.initialView(g);
  assert.deepEqual(idsOf(g, view), ["module:com.acme:api", "module:com.acme:core", "module:com.acme:store"]);
  const dep = g.edgeKinds.indexOf("depends_on");
  const links = [];
  for (let e = 0; e < g.m; e++) {
    if (view.has(g.src[e]) && view.has(g.dst[e])) {
      assert.equal(g.ekind[e], dep);
      links.push(g.ids[g.src[e]] + ">" + g.ids[g.dst[e]]);
    }
  }
  assert.deepEqual(links.sort(), [
    "module:com.acme:api>module:com.acme:core",
    "module:com.acme:store>module:com.acme:core",
  ]);
});

test("REQ025_AC5 vue initiale sans module : 100 nœuds de plus haut degré", () => {
  const data = helpers.noModuleGraph(150);
  const g = model.load(data);
  const deg = new Array(150).fill(0);
  for (const [s, d] of data.edges) {
    deg[s]++;
    deg[d]++;
  }
  const expected = [...deg.keys()]
    .sort((a, b) => deg[b] - deg[a] || (data.nodes[a][0] < data.nodes[b][0] ? -1 : 1))
    .slice(0, 100);
  const view = model.initialView(g);
  assert.equal(view.size, 100);
  assert.deepEqual([...view].sort((a, b) => a - b), expected.sort((a, b) => a - b));
});

test("REQ025_AC5 vue initiale : graphe vide", () => {
  const g = model.load({ nodes: [], edges: [], nodeKinds: [], edgeKinds: [] });
  assert.equal(model.initialView(g).size, 0);
});

test("REQ025_AC6 recherche orderserv : OrderService en tête", () => {
  const g = model.load(helpers.loadMaven3());
  const r = model.search(g, "orderserv");
  assert.equal(g.ids[r[0]], ORDER);
  assert.deepEqual(model.search(g, "ORDERSERV"), r);
  assert.deepEqual(model.search(g, "  "), []);
  assert.deepEqual(model.search(g, "zzzzzz"), []);
});

test("REQ025_AC6 classement exact, préfixe, sous-chaîne, identifiant", () => {
  const mk = (id, short, q) => [id, 0, short, q, null, null, null];
  const g = model.load({
    nodeKinds: ["type"],
    edgeKinds: [],
    edges: [],
    nodes: [
      mk("type:z.inner", "XfooY", "z.XfooY"),
      mk("type:q.Foo2", "Foo2", "q.Foo2"),
      mk("type:b.foo", "foo", "b.foo"),
      mk("type:a.foo", "FOO", "a.foo"),
      mk("type:c.Other", "Other", "c.foo.Other"),
      mk("type:d.bar", "bar", "d.bar"),
    ],
  });
  const r = model.search(g, "foo").map((i) => g.ids[i]);
  assert.deepEqual(r, ["type:a.foo", "type:b.foo", "type:q.Foo2", "type:c.Other", "type:z.inner"]);
});

test("REQ025_AC6 au plus 20 résultats", () => {
  const g = model.load(helpers.randomGraph(200, 0));
  assert.equal(model.search(g, "c").length, 20);
});

test("REQ025_AC8 détails de OrderService", () => {
  const data = helpers.loadMaven3();
  const g = model.load(data);
  const i = g.ids.indexOf(ORDER);
  const d = model.details(g, i);
  assert.equal(d.id, ORDER);
  assert.equal(d.kind, "type");
  assert.equal(d.subkind, "class");
  assert.equal(d.qualified, "com.acme.core.OrderService");
  assert.equal(d.file, "core/src/main/java/com/acme/core/OrderService.java");
  assert.equal(d.summary, "Place les commandes.");
  const out = {};
  const inn = {};
  for (const [s, t, k] of data.edges) {
    const kn = data.edgeKinds[k];
    if (s === i) (out[kn] = out[kn] || []).push(data.nodes[t][0]);
    if (t === i) (inn[kn] = inn[kn] || []).push(data.nodes[s][0]);
  }
  for (const o of [out, inn]) for (const k of Object.keys(o)) o[k].sort();
  assert.deepEqual(d.out, out);
  assert.deepEqual(d.in, inn);
  assert.ok(Object.keys(d.out).length + Object.keys(d.in).length > 0);
});

test("REQ025_AC8 détails : nœud inconnu et champs absents", () => {
  const g = model.load(helpers.loadMaven3());
  assert.throws(() => model.details(g, 99999), RangeError);
  const d = model.details(g, g.ids.indexOf("concept:persistance"));
  assert.equal(d.summary, null);
  assert.equal(d.file, null);
});
