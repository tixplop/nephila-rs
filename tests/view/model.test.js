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
  const { view, hidden } = model.initialView(g);
  assert.equal(hidden, 0);
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
  const { view, hidden } = model.initialView(g);
  assert.equal(hidden, 0);
  assert.equal(view.size, 100);
  assert.deepEqual([...view].sort((a, b) => a - b), expected.sort((a, b) => a - b));
});

test("REQ025_AC5 vue initiale : graphe vide", () => {
  const g = model.load({ nodes: [], edges: [], nodeKinds: [], edgeKinds: [] });
  const r = model.initialView(g);
  assert.equal(r.view.size, 0);
  assert.equal(r.hidden, 0);
});

test("REQ025_AC5 600 modules : les 100 de plus haut degré, 500 non affichés, extension possible", () => {
  const data = helpers.moduleGraph(600);
  const g = model.load(data);
  const modules = [];
  for (let i = 0; i < g.n; i++) if (g.kind[i] === 0) modules.push(i);
  assert.equal(modules.length, 600);
  const expected = modules
    .sort((a, b) => g.degree[b] - g.degree[a] || (g.ids[a] < g.ids[b] ? -1 : 1))
    .slice(0, 100)
    .sort((a, b) => a - b);
  const { view, hidden } = model.initialView(g);
  assert.equal(view.size, 100);
  assert.equal(hidden, 500);
  assert.deepEqual([...view].sort((a, b) => a - b), expected);
  const m = expected[0];
  const r = model.expand(g, view, m);
  assert.ok(r.view.size > 100 && r.view.size <= 300);
  assert.ok(model.visible(g, view, {}).nodes.every((i) => g.kind[i] === 0));
  assert.deepEqual([...view].sort((a, b) => a - b), expected, "vue d'origine intacte");
});

test("REQ025_AC5 limite explicite et égalités par identifiant", () => {
  const g = model.load(helpers.moduleGraph(30));
  const a = model.initialView(g, 10);
  assert.equal(a.view.size, 10);
  assert.equal(a.hidden, 20);
  assert.deepEqual([...model.initialView(g, 10).view], [...a.view]);
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

test("REQ025_AC11 indexOf retrouve l'indice d'un identifiant, -1 si inconnu", () => {
  const g = model.load(helpers.loadMaven3());
  assert.equal(model.indexOf(g, ORDER) >= 0, true);
  assert.equal(g.ids[model.indexOf(g, ORDER)], ORDER);
  assert.equal(model.indexOf(g, "type:inconnu"), -1);
});

test("REQ025_AC11 navigate : nœud déjà affiché, vue inchangée et message remis à zéro", () => {
  const g = model.load(helpers.starGraph(10));
  const v = new Set([0, 1]);
  const r = model.navigate(g, v, 1);
  assert.deepEqual([...r.view].sort(), [0, 1]);
  assert.equal(r.hidden, 0);
  assert.equal(r.recentered, false);
});

test("REQ025_AC11 navigate : nœud absent ajouté à la vue sous le plafond", () => {
  const g = model.load(helpers.starGraph(10));
  const v = new Set([0]);
  const r = model.navigate(g, v, 3);
  assert.deepEqual([...r.view].sort((a, b) => a - b), [0, 3]);
  assert.equal(r.hidden, 0);
  assert.equal(r.recentered, false);
  assert.equal(v.size, 1, "la vue d'origine n'est pas modifiée");
  assert.throws(() => model.navigate(g, v, 99), RangeError);
});

test("REQ025_AC11 navigate : vue pleine, recentrage sur le nœud et ses voisins avec le compte masqué", () => {
  const g = model.load(helpers.starGraph(500));
  const full = new Set();
  for (let k = 1; k <= 300; k++) full.add(k);
  const r = model.navigate(g, full, 0);
  assert.equal(r.view.size, 300);
  assert.ok(r.view.has(0));
  assert.equal(r.hidden, 201);
  assert.equal(r.recentered, true);
  assert.equal(model.navigate(g, full, 0, 400).recentered, false);
  assert.equal(full.size, 300, "la vue d'origine n'est pas modifiée");
  assert.ok(!full.has(0));
});

test("REQ025_AC11 navigate : étoile de 500, vue de 300 feuilles sans le centre", () => {
  const g = model.load(helpers.starGraph(500));
  const full = new Set();
  for (let k = 1; k <= 300; k++) full.add(k);
  const r = model.navigate(g, full, 0);
  assert.equal(r.view.size, 300);
  assert.ok(r.view.has(0));
  assert.equal(r.hidden, 201);
  assert.equal(r.recentered, true);
  assert.throws(() => model.navigate(g, full, -1), RangeError);
  assert.throws(() => model.navigate(g, full, 1.5), RangeError);
});

// ---- REQ-039 : allView ----

function degreeRank(g) {
  const idx = [...Array(g.n).keys()];
  idx.sort((a, b) => g.degree[b] - g.degree[a] || (g.ids[a] < g.ids[b] ? -1 : g.ids[a] > g.ids[b] ? 1 : 0));
  return idx;
}

test("REQ039_AC1 allView sur maven3 : tous les nœuds, tous les liens", () => {
  const data = helpers.loadMaven3();
  const g = model.load(data);
  const r = model.allView(g);
  assert.equal(r.hidden, 0);
  assert.equal(r.view.size, g.n);
  const v = model.visible(g, r.view);
  assert.equal(v.nodes.length, data.nodes.length);
  assert.equal(v.edges.length, data.edges.length);
});

test("REQ039_AC1 allView sur un graphe vide", () => {
  const g = model.load({ nodes: [], edges: [], nodeKinds: [], edgeKinds: [] });
  const r = model.allView(g);
  assert.equal(r.view.size, 0);
  assert.equal(r.hidden, 0);
});

test("REQ039_AC2 allView : 6000 nœuds, les 5000 de plus haut degré", () => {
  const g = model.load(helpers.degreeGraph(6000));
  const rank = degreeRank(g);
  assert.equal(g.degree[rank[4999]], g.degree[rank[5000]], "la coupure doit traverser une égalité");
  assert.ok(g.degree[rank[0]] > g.degree[rank[1]]);
  const r = model.allView(g);
  assert.equal(model.ALL_LIMIT, 5000);
  assert.equal(r.hidden, 1000);
  assert.deepEqual([...r.view].sort((a, b) => a - b), rank.slice(0, 5000).sort((a, b) => a - b));
});

test("REQ039_AC2 allView : plafond explicite et plafond égal au nombre de nœuds", () => {
  const g = model.load(helpers.loadMaven3());
  const r = model.allView(g, 10);
  assert.equal(r.view.size, 10);
  assert.equal(r.hidden, g.n - 10);
  assert.deepEqual([...r.view].sort((a, b) => a - b), degreeRank(g).slice(0, 10).sort((a, b) => a - b));
  const full = model.allView(g, g.n);
  assert.equal(full.view.size, g.n);
  assert.equal(full.hidden, 0);
  const g2 = model.load(helpers.noModuleGraph(40));
  for (const cap of [1, 7, 39]) {
    const r2 = model.allView(g2, cap);
    assert.deepEqual([...r2.view].sort((a, b) => a - b), degreeRank(g2).slice(0, cap).sort((a, b) => a - b));
    assert.equal(r2.hidden, 40 - cap);
  }
});

test("REQ039_AC2 allView : plafonds invalides", () => {
  const g = model.load(helpers.loadMaven3());
  for (const cap of [0, -1, 1.5]) {
    assert.throws(() => model.allView(g, cap), RangeError);
    assert.throws(() => model.expand(g, new Set(), 0, cap), RangeError);
  }
});

test("REQ039_AC3 mode ensemble : expand et navigate au plafond de 5000", () => {
  const g = model.load(helpers.degreeGraph(6000));
  const { view } = model.allView(g);
  const copy = new Set(view);
  // nœud affiché dont des voisins manquent
  let i = -1;
  for (const x of view) {
    const nb = new Set();
    for (let p = g.out.start[x]; p < g.out.start[x + 1]; p++) nb.add(g.dst[g.out.list[p]]);
    for (let p = g.in.start[x]; p < g.in.start[x + 1]; p++) nb.add(g.src[g.in.list[p]]);
    nb.delete(x);
    const missing = [...nb].filter((y) => !view.has(y)).length;
    if (missing > 0) {
      i = x;
      const r = model.expand(g, view, x, 5000);
      assert.deepEqual([...r.view].sort((a, b) => a - b), [...view].sort((a, b) => a - b));
      assert.equal(r.hidden, missing);
      break;
    }
  }
  assert.ok(i >= 0, "un nœud affiché doit avoir un voisin absent");
  // navigation vers un absent
  let j = -1;
  for (let x = 0; x < g.n; x++) if (!view.has(x)) { j = x; break; }
  const nav = model.navigate(g, view, j, 5000);
  assert.equal(nav.recentered, true);
  assert.ok(nav.view.has(j));
  assert.ok(nav.view.size <= 5000);
  const expected = model.neighborsView(g, j, 5000);
  assert.deepEqual([...nav.view].sort((a, b) => a - b), [...expected.view].sort((a, b) => a - b));
  assert.deepEqual([...view].sort((a, b) => a - b), [...copy].sort((a, b) => a - b));
});

test("REQ039_AC3 mode ensemble : 1000 nœuds, navigate ne change rien", () => {
  const g = model.load(helpers.noModuleGraph(1000));
  const { view } = model.allView(g);
  const copy = new Set(view);
  for (let j = 0; j < g.n; j++) {
    const r = model.navigate(g, view, j, 5000);
    assert.equal(r.view.size, 1000);
    assert.equal(r.hidden, 0);
    assert.equal(r.recentered, false);
  }
  assert.deepEqual(view, copy);
});

test("REQ039_AC4 allView et visible sur 120 000 nœuds", () => {
  const g = model.load(helpers.randomGraph(120000, 300000));
  let t = performance.now();
  const r = model.allView(g);
  const tAll = performance.now() - t;
  t = performance.now();
  const v = model.visible(g, r.view);
  const tVis = performance.now() - t;
  assert.equal(r.view.size, 5000);
  assert.equal(r.hidden, 115000);
  assert.ok(v.nodes.length === 5000);
  assert.ok(tAll < 200, "allView " + tAll);
  assert.ok(tVis < 100, "visible " + tVis);
});

// ---- REQ-039 : layoutStep ----

// Référence : tick() de app.js 0.13.0 (répulsion sur toutes les paires), copie littérale de la logique.
function referenceStep(p, edges, alpha, pinned) {
  const n = p.length;
  let a, b, dx, dy, d2, d, f;
  for (a = 0; a < n; a++) {
    for (b = a + 1; b < n; b++) {
      dx = p[a].x - p[b].x;
      dy = p[a].y - p[b].y;
      d2 = dx * dx + dy * dy + 0.01;
      if (d2 > 90000) continue;
      f = (2000 * alpha) / d2;
      d = Math.sqrt(d2);
      p[a].vx += (dx / d) * f * d;
      p[a].vy += (dy / d) * f * d;
      p[b].vx -= (dx / d) * f * d;
      p[b].vy -= (dy / d) * f * d;
    }
  }
  edges.forEach(function (e) {
    const s = p[e[0]];
    const t = p[e[1]];
    dx = t.x - s.x;
    dy = t.y - s.y;
    d = Math.sqrt(dx * dx + dy * dy) + 0.01;
    f = (d - 70) * 0.05 * alpha;
    s.vx += (dx / d) * f;
    s.vy += (dy / d) * f;
    t.vx -= (dx / d) * f;
    t.vy -= (dy / d) * f;
  });
  for (a = 0; a < n; a++) {
    p[a].vx -= p[a].x * 0.01 * alpha;
    p[a].vy -= p[a].y * 0.01 * alpha;
    if (a === pinned) {
      p[a].vx = 0;
      p[a].vy = 0;
      continue;
    }
    p[a].vx *= 0.6;
    p[a].vy *= 0.6;
    p[a].x += p[a].vx;
    p[a].y += p[a].vy;
  }
  alpha *= 0.985;
  return alpha < 0.005 ? 0 : alpha;
}

function randomPoints(n, rnd, spread, offsetX = 0, offsetY = 0) {
  const p = [];
  for (let i = 0; i < n; i++) p.push({ x: (rnd() - 0.5) * spread + offsetX, y: (rnd() - 0.5) * spread + offsetY, vx: 0, vy: 0 });
  return p;
}

function randomEdges(n, m, rnd) {
  const e = [];
  for (let k = 0; k < m; k++) e.push([Math.floor(rnd() * n), Math.floor(rnd() * n)]);
  return e;
}

function assertSame(p, q, label) {
  assert.equal(p.length, q.length);
  for (let i = 0; i < p.length; i++) {
    for (const k of ["x", "y", "vx", "vy"]) {
      assert.ok(Math.abs(p[i][k] - q[i][k]) <= 1e-6, label + " nœud " + i + " " + k + " : " + p[i][k] + " vs " + q[i][k]);
    }
  }
}

function compareSteps(p0, edges, steps, pinned, label) {
  const a = p0.map((x) => ({ ...x }));
  const b = p0.map((x) => ({ ...x }));
  let al = 1;
  let bl = 1;
  for (let s = 0; s < steps; s++) {
    al = model.layoutStep(a, edges, al, pinned);
    bl = referenceStep(b, edges, bl, pinned);
    assert.equal(al, bl);
    assertSame(a, b, label + " pas " + s);
  }
}

test("REQ039_AC5 layoutStep égal au calcul par paires (300 nœuds, 600 liens)", () => {
  const rnd = helpers.lcg(7);
  const p = randomPoints(300, rnd, 1200);
  compareSteps(p, randomEdges(300, 600, rnd), 6, -1, "aléatoire");
});

test("REQ039_AC5 layoutStep : cas limites de la grille", () => {
  const mk = (xs) => xs.map(([x, y]) => ({ x, y, vx: 0, vy: 0 }));
  compareSteps(mk([[0, 0], [300, 0]]), [], 3, -1, "300 exact");
  compareSteps(mk([[299.99, 0], [0.01, 0]]), [], 3, -1, "juste sous 300");
  compareSteps(mk([[-1, -1], [1, 1], [-299, 0], [-301, 5], [299, -299]]), [[0, 1], [2, 3]], 4, -1, "négatives");
  const rnd = helpers.lcg(11);
  compareSteps(randomPoints(60, rnd, 1400, -700, -700), randomEdges(60, 90, rnd), 4, -1, "négatives aléatoires");
  compareSteps(randomPoints(80, rnd, 200, 5, 5), randomEdges(80, 120, rnd), 4, -1, "une seule case");
  compareSteps(randomPoints(50, rnd, 900), randomEdges(50, 70, rnd), 4, 3, "épinglé");
});

test("REQ039_AC5 layoutStep : 5000 nœuds, un pas < 50 ms", () => {
  const rnd = helpers.lcg(3);
  const p = [];
  for (let i = 0; i < 5000; i++) {
    const r = 2000 * Math.sqrt(rnd());
    const th = 2 * Math.PI * rnd();
    p.push({ x: r * Math.cos(th), y: r * Math.sin(th), vx: 0, vy: 0 });
  }
  const edges = randomEdges(5000, 15000, rnd);
  let alpha = model.layoutStep(p, edges, 1, -1); // échauffement
  const times = [];
  for (let s = 0; s < 5; s++) {
    const t = performance.now();
    alpha = model.layoutStep(p, edges, alpha, -1);
    times.push(performance.now() - t);
  }
  times.sort((a, b) => a - b);
  assert.ok(times[2] < 50, "médiane " + times[2]);
});

test("REQ039_AC5 layoutStep : nœud épinglé immobile, alpha décroît jusqu'à 0", () => {
  const rnd = helpers.lcg(5);
  const p = randomPoints(20, rnd, 300);
  const edges = randomEdges(20, 30, rnd);
  const x0 = p[4].x;
  const y0 = p[4].y;
  let alpha = 1;
  const seen = [];
  for (let s = 0; s < 400 && alpha > 0; s++) {
    alpha = model.layoutStep(p, edges, alpha, 4);
    seen.push(alpha);
    assert.equal(p[4].x, x0);
    assert.equal(p[4].y, y0);
    assert.equal(p[4].vx, 0);
    assert.equal(p[4].vy, 0);
  }
  assert.equal(alpha, 0);
  assert.ok(Math.abs(seen[0] - 0.985) < 1e-12);
  assert.ok(Math.abs(seen[1] - 0.985 * 0.985) < 1e-12);
  let ref = 1;
  let steps = 0;
  while (ref >= 0.005) {
    ref *= 0.985;
    steps++;
  }
  assert.equal(seen.length, steps);
  assert.ok(seen[steps - 2] >= 0.005);
});

// ---- REQ-040 : liens masqués ----

test("REQ040_AC1 linksShown : état initial et délai après la molette", () => {
  assert.equal(model.LINKS_DELAY_MS, 150);
  let m = model.motionState();
  assert.equal(model.linksShown(m, 0), true);
  m = model.onWheel(m, 1000);
  assert.equal(model.linksShown(m, 1000), false);
  assert.equal(model.linksShown(m, 1149), false);
  assert.equal(model.linksShown(m, 1150), true);
  assert.equal(model.linksDueAt(m), 1150);
  m = model.onWheel(m, 1100);
  assert.equal(model.linksShown(m, 1149), false);
  assert.equal(model.linksShown(m, 1249), false);
  assert.equal(model.linksShown(m, 1250), true);
  assert.equal(model.linksDueAt(m), 1250);
});

test("REQ040_AC2 linksShown : déplacement du fond", () => {
  const m0 = model.motionState();
  const m = model.onPanStart(m0);
  assert.equal(model.linksShown(m, 0), false);
  assert.equal(model.linksShown(m, 10000), false);
  assert.equal(model.linksDueAt(m), null);
  const e = model.onPanEnd(m);
  assert.equal(model.linksShown(e, 10000), true);
  // fin pendant le délai d'une molette
  let w = model.onPanStart(model.motionState());
  w = model.onWheel(w, 9950);
  w = model.onPanEnd(w);
  assert.equal(model.linksShown(w, 10000), false);
  assert.equal(model.linksShown(w, 10099), false);
  assert.equal(model.linksShown(w, 10100), true);
  assert.equal(model.linksDueAt(w), 10100);
  // fonctions pures : l'état reçu n'est pas modifié ; état initial inchangé
  assert.deepEqual(m0, { lastWheel: -Infinity, panning: false });
  assert.deepEqual(m, { lastWheel: -Infinity, panning: true });
});

// ---- REQ-150 : couleur par communauté ----

const PALETTE8 = ["#36b", "#c60", "#393", "#c33", "#739", "#088", "#855", "#c39"];

// Données de version 2 : n nœuds `type`, communautés de tailles données ; le nœud i a le numéro nums[i].
function withCommunities(count, sizes) {
  const data = helpers.noModuleGraph(count);
  data.version = 2;
  data.communities = sizes.map((s, c) => ["C" + c, s]);
  data.nodes.forEach((n, i) => n.push(i < sizes.length ? i : null));
  return data;
}

test("REQ150_AC1 AC-150-1 couleur d'un nœud : palette si numéro < K, neutre sinon, genre par défaut", () => {
  const g = model.load(withCommunities(12, new Array(10).fill(3)));
  for (let i = 0; i < 8; i++) assert.equal(model.nodeColor(g, i, "community"), PALETTE8[i]);
  assert.equal(model.nodeColor(g, 8, "community"), "#888");
  assert.equal(model.nodeColor(g, 9, "community"), "#888");
  assert.equal(model.nodeColor(g, 11, "community"), "#888"); // sans numéro
  // mode genre : couleur du genre, comme à la 0.24.4
  const k = model.load(helpers.loadMaven3());
  for (let i = 0; i < k.n; i++) {
    assert.equal(model.nodeColor(k, i, "kind"), model.KIND_PALETTE[k.kind[i] % model.KIND_PALETTE.length]);
  }
  assert.deepEqual(model.KIND_PALETTE, ["#4e79a7", "#f28e2b", "#e15759", "#76b7b2", "#59a14f", "#edc948", "#b07aa1", "#ff9da7", "#9c755f", "#7f7f7f"]);
  // mode communauté sur la fixture : palette si numéro < 8, neutre sinon
  for (let i = 0; i < k.n; i++) {
    const c = k.community[i];
    assert.equal(model.nodeColor(k, i, "community"), c >= 0 && c < 8 ? PALETTE8[c] : "#888");
  }
});

test("REQ150_AC1 AC-150-1 la fixture maven3 régénérée porte ses communautés", () => {
  const data = helpers.loadMaven3();
  assert.equal(data.version, 2);
  const g = model.load(data);
  assert.ok(g.communities.length > 0);
  const legend = model.communityLegend(g);
  assert.equal(legend.length, g.communities.length); // 6 communautés : pas d'« autres »
  legend.forEach((e, c) => {
    assert.equal(e.color, PALETTE8[c]);
    assert.equal(e.text, c + " " + g.communities[c][0] + " (" + g.communities[c][1] + " nœuds)");
  });
});

test("REQ150_AC7 AC-150-7 palette et couleur neutre", () => {
  assert.deepEqual(model.COMMUNITY_PALETTE, PALETTE8);
  assert.equal(model.COMMUNITY_OTHER, "#888");
});

test("REQ150_AC7 AC-150-7 10 communautés : 9 entrées, les 8 premières puis « autres »", () => {
  const sizes = [20, 18, 16, 14, 12, 10, 8, 6, 4, 3];
  const g = model.load(withCommunities(12, sizes));
  const legend = model.communityLegend(g);
  assert.equal(legend.length, 9);
  for (let c = 0; c < 8; c++) {
    assert.deepEqual(legend[c], { color: PALETTE8[c], text: c + " C" + c + " (" + sizes[c] + " nœuds)" });
  }
  assert.deepEqual(legend[8], { color: "#888", text: "autres (7 nœuds)" });
});

test("REQ150_AC7 AC-150-7 8 communautés ou moins : pas d'entrée « autres »", () => {
  assert.equal(model.communityLegend(model.load(withCommunities(10, new Array(8).fill(1)))).length, 8);
  assert.equal(model.communityLegend(model.load(withCommunities(10, [5]))).length, 1);
});

test("REQ150_AC3 AC-150-3 détails : numéro et nom de la communauté de OrderService", () => {
  const g = model.load(helpers.loadMaven3());
  const i = model.indexOf(g, ORDER);
  const d = model.details(g, i);
  const c = g.community[i];
  assert.ok(c >= 0);
  assert.equal(d.community.number, c);
  assert.equal(d.community.name, g.communities[c][0]);
  assert.equal(d.community.text, c + " " + g.communities[c][0]);
});

test("REQ150_AC3 AC-150-3 sans communautés : choix désactivé, détails sans communauté", () => {
  const data = helpers.noModuleGraph(5);
  const g = model.load(data); // version 1 : ni `communities` ni numéros
  assert.deepEqual(model.colorChoice(g), {
    enabled: false,
    note: "Communautés non calculées : lancez nephila update, puis exportez de nouveau la page",
  });
  assert.equal(model.details(g, 0).community, null);
  const n = model.load({ ...data, version: 2, communities: [] });
  assert.equal(model.colorChoice(n).enabled, false);
});

test("REQ150_AC7 AC-150-7 graphe vide : choix désactivé, note vide ; avec communautés : activé", () => {
  const empty = model.load({ version: 2, name: "x", nodeKinds: [], edgeKinds: [], nodes: [], edges: [], communities: [] });
  assert.deepEqual(model.colorChoice(empty), { enabled: false, note: "" });
  assert.deepEqual(model.colorChoice(model.load(withCommunities(4, [2, 2]))), { enabled: true, note: "" });
});

test("REQ150_AC7 AC-150-7 un numéro hors liste ou invalide n'a pas de communauté", () => {
  const data = withCommunities(3, [1, 1]);
  data.nodes[2][7] = 99;
  data.nodes[1][7] = -4;
  const g = model.load(data);
  assert.equal(model.details(g, 2).community, null);
  assert.equal(model.details(g, 1).community, null);
  assert.equal(model.nodeColor(g, 2, "community"), "#888");
});

test("REQ150_AC4 AC-150-4 120 000 nœuds avec communautés : chargement, couleurs et légende rapides", () => {
  const data = helpers.randomGraph(120000, 300000);
  data.version = 2;
  data.communities = Array.from({ length: 5000 }, (_, c) => ["C" + c, 24]);
  data.nodes.forEach((n, i) => n.push(i % 5000));
  let t = performance.now();
  const g = model.load(data);
  const tLoad = performance.now() - t;
  t = performance.now();
  let acc = 0;
  for (let i = 0; i < g.n; i++) acc += model.nodeColor(g, i, "community").length;
  const tColor = performance.now() - t;
  const legend = model.communityLegend(g);
  assert.ok(acc > 0);
  assert.equal(legend.length, 9);
  assert.equal(legend[8].text, "autres (" + 4992 * 24 + " nœuds)");
  t = performance.now();
  assert.ok(model.search(g, "c1234").length > 0);
  const tSearch = performance.now() - t;
  let hub = 0;
  for (let i = 1; i < g.n; i++) if (g.degree[i] > g.degree[hub]) hub = i;
  t = performance.now();
  model.expand(g, new Set([hub]), hub);
  const tExpand = performance.now() - t;
  t = performance.now();
  const all = model.allView(g);
  const tAll = performance.now() - t;
  t = performance.now();
  model.visible(g, all.view, {});
  const tVisible = performance.now() - t;
  assert.ok(tSearch < 100, "search " + tSearch);
  assert.ok(tExpand < 50, "expand " + tExpand);
  assert.ok(tAll < 200, "allView " + tAll);
  assert.ok(tVisible < 100, "visible " + tVisible);
  assert.ok(tLoad < 2000, "load " + tLoad);
  assert.ok(tColor < 200, "couleurs " + tColor);
});

function luminance(hex) {
  const h = hex.length === 4 ? "#" + [...hex.slice(1)].map((c) => c + c).join("") : hex;
  const ch = [1, 3, 5].map((p) => parseInt(h.slice(p, p + 2), 16) / 255);
  const [r, g, b] = ch.map((c) => (c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4));
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

test("REQ150_AC7 contraste des 8 couleurs et du neutre sur le fond du canevas >= 3:1", () => {
  const bg = luminance("#fafafa");
  for (const c of [...model.COMMUNITY_PALETTE, model.COMMUNITY_OTHER]) {
    const ratio = (bg + 0.05) / (luminance(c) + 0.05);
    assert.ok(ratio >= 3, c + " : " + ratio);
  }
});
