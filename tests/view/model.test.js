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

test("REQ025_AC7 AC-025-7 extension : voisins entrants et sortants", () => {
  const g = model.load(helpers.loadMaven3());
  const i = g.ids.indexOf(ORDER);
  const r = model.expand(g, new Set([i]), i);
  const d = model.details(g, i);
  const expected = new Set([ORDER]);
  for (const o of [d.out, d.in]) for (const k of Object.keys(o)) o[k].forEach((x) => expected.add(x));
  assert.deepEqual(idsOf(g, r.view), [...expected].sort());
  assert.equal(r.hidden, 0);
  const vis = model.visible(g, r.view, {});
  for (const e of vis.edges) assert.ok(r.view.has(g.src[e]) && r.view.has(g.dst[e]));
  assert.throws(() => model.expand(g, new Set(), 99999), RangeError);
  assert.throws(() => model.expand(g, new Set(), i, 0), RangeError);
});

test("REQ025_AC7 AC-025-7 plafond : 500 voisins, 300 nœuds, 201 non affichés", () => {
  const data = helpers.starGraph(500);
  // degré supplémentaire pour S00500 et S00499 : ils doivent être retenus d'abord
  data.nodes.push(["type:p.X", 0, "X", "p.X", null, null, null]);
  data.edges.push([500, 501, 0], [499, 501, 0], [499, 501, 0]);
  const g = model.load(data);
  const r = model.expand(g, new Set([0]), 0);
  assert.equal(r.view.size, 300);
  assert.equal(r.hidden, 201);
  assert.ok(r.view.has(499) && r.view.has(500));
  assert.ok(r.view.has(1) && r.view.has(297) && !r.view.has(298));
  const n = model.neighborsView(g, 0);
  assert.equal(n.view.size, 300);
  assert.equal(n.hidden, 201);
  assert.equal(model.expand(g, new Set([0]), 0, 10).view.size, 10);
  // vue déjà au plafond : rien n'est ajouté
  const full = model.expand(g, r.view, 0);
  assert.equal(full.view.size, 300);
  assert.equal(full.hidden, 201);
});

test("REQ025_AC7 AC-025-7 plafond : nœud hors d'une vue pleine", () => {
  const g = model.load(helpers.starGraph(500));
  const full = new Set();
  for (let k = 1; k <= 300; k++) full.add(k);
  const r = model.expand(g, full, 0);
  assert.ok(r.view.size <= 300);
  assert.equal(r.view.size, 300);
  assert.ok(!r.view.has(0));
  assert.equal(r.hidden, 200);
});

test("REQ025_AC9 AC-025-9 filtres par genre de nœud et de lien", () => {
  const g = model.load(randomGraphSmall());
  const view = new Set();
  for (let i = 0; i < g.n; i++) view.add(i);
  const all = model.visible(g, view, {});
  assert.equal(all.nodes.length, g.n);
  assert.equal(all.edges.length, g.m);
  const noMethod = model.visible(g, view, { hiddenNodeKinds: ["method"] });
  assert.ok(noMethod.nodes.every((i) => g.nodeKinds[g.kind[i]] !== "method"));
  assert.ok(noMethod.nodes.length < g.n);
  const ok = new Set(noMethod.nodes);
  assert.ok(noMethod.edges.every((e) => ok.has(g.src[e]) && ok.has(g.dst[e])));
  const expectedEdges = [];
  for (let e = 0; e < g.m; e++) if (ok.has(g.src[e]) && ok.has(g.dst[e])) expectedEdges.push(e);
  assert.deepEqual(noMethod.edges, expectedEdges);
  const noCalls = model.visible(g, view, { hiddenEdgeKinds: ["calls"] });
  assert.equal(noCalls.nodes.length, g.n);
  assert.ok(noCalls.edges.length < g.m);
  assert.ok(noCalls.edges.every((e) => g.edgeKinds[g.ekind[e]] !== "calls"));
  const back = model.visible(g, view, { hiddenNodeKinds: [], hiddenEdgeKinds: [] });
  assert.deepEqual(back, all);
  // liens limités aux nœuds de la vue
  const sub = model.visible(g, new Set([0, 1]), {});
  assert.ok(sub.edges.every((e) => g.src[e] <= 1 && g.dst[e] <= 1));
});

function randomGraphSmall() {
  return helpers.randomGraph(60, 200);
}

test("REQ025_AC10 AC-025-10 performances sur 120 000 nœuds et 300 000 liens", () => {
  const data = helpers.randomGraph(120000, 300000);
  let t = performance.now();
  const g = model.load(data);
  const tLoad = performance.now() - t;
  t = performance.now();
  const hits = model.search(g, "c1234");
  const tSearch = performance.now() - t;
  assert.ok(hits.length > 0);
  let hub = 0;
  for (let i = 1; i < g.n; i++) if (g.degree[i] > g.degree[hub]) hub = i;
  t = performance.now();
  model.expand(g, new Set([hub]), hub);
  const tExpand = performance.now() - t;
  assert.ok(tLoad < 2000, "load " + tLoad);
  assert.ok(tSearch < 100, "search " + tSearch);
  assert.ok(tExpand < 50, "expand " + tExpand);
});

test("REQ025_AC10 AC-025-10 extension d'un nœud de 5000 voisins", () => {
  const g = model.load(helpers.starGraph(5000));
  const t = performance.now();
  const r = model.expand(g, new Set([0]), 0);
  const d = performance.now() - t;
  assert.equal(r.view.size, 300);
  assert.equal(r.hidden, 4701);
  assert.ok(d < 50, "expand " + d);
});
