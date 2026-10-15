"use strict";
// Logique de vue pure (sans DOM) : chargement, index, vue initiale, recherche, détails.

var INITIAL_LIMIT = 100;
var SEARCH_LIMIT = 20;

function readData(text) {
  return JSON.parse(text);
}

function cmp(a, b) {
  return a < b ? -1 : a > b ? 1 : 0;
}

// Adjacence en CSR : start[i]..start[i + 1] indexe `list` (indices de liens).
function buildCsr(n, ends) {
  var start = new Int32Array(n + 1);
  var m = ends.length;
  for (var e = 0; e < m; e++) start[ends[e] + 1]++;
  for (var i = 0; i < n; i++) start[i + 1] += start[i];
  var fill = start.slice(0, n);
  var list = new Int32Array(m);
  for (var k = 0; k < m; k++) list[fill[ends[k]]++] = k;
  return { start: start, list: list };
}

function load(data) {
  if (!data || !Array.isArray(data.nodes) || !Array.isArray(data.edges)) {
    throw new Error("données de graphe invalides");
  }
  var nodeKinds = data.nodeKinds || [];
  var edgeKinds = data.edgeKinds || [];
  var n = data.nodes.length;
  var m = data.edges.length;
  var g = {
    name: data.name,
    nodeKinds: nodeKinds,
    edgeKinds: edgeKinds,
    n: n,
    m: m,
    ids: new Array(n),
    kind: new Int32Array(n),
    shortName: new Array(n),
    qualified: new Array(n),
    file: new Array(n),
    subkind: new Array(n),
    summary: new Array(n),
    src: new Int32Array(m),
    dst: new Int32Array(m),
    ekind: new Int32Array(m),
    degree: new Int32Array(n),
    lowShort: new Array(n),
    lowQualified: new Array(n),
  };
  for (var i = 0; i < n; i++) {
    var r = data.nodes[i];
    g.ids[i] = r[0];
    g.kind[i] = r[1];
    g.shortName[i] = r[2];
    g.qualified[i] = r[3];
    g.file[i] = r[4] === undefined ? null : r[4];
    g.subkind[i] = r[5] === undefined ? null : r[5];
    g.summary[i] = r[6] === undefined ? null : r[6];
    g.lowShort[i] = String(r[2]).toLowerCase();
    g.lowQualified[i] = String(r[3]).toLowerCase();
  }
  for (var e = 0; e < m; e++) {
    var s = data.edges[e][0];
    var d = data.edges[e][1];
    if (!(s >= 0 && s < n && d >= 0 && d < n)) {
      throw new Error("lien " + e + " : extrémité hors limites");
    }
    g.src[e] = s;
    g.dst[e] = d;
    g.ekind[e] = data.edges[e][2];
    g.degree[s]++;
    g.degree[d]++;
  }
  g.out = buildCsr(n, g.src);
  g.in = buildCsr(n, g.dst);
  return g;
}

function initialView(g) {
  var view = new Set();
  var moduleKind = g.nodeKinds.indexOf("module");
  if (moduleKind >= 0) {
    for (var i = 0; i < g.n; i++) if (g.kind[i] === moduleKind) view.add(i);
    if (view.size > 0) return view;
  }
  var order = [];
  for (var j = 0; j < g.n; j++) order.push(j);
  order.sort(function (a, b) {
    return g.degree[b] - g.degree[a] || cmp(g.ids[a], g.ids[b]);
  });
  order.slice(0, INITIAL_LIMIT).forEach(function (x) {
    view.add(x);
  });
  return view;
}

function search(g, text) {
  var q = String(text == null ? "" : text).trim().toLowerCase();
  if (q === "") return [];
  var hits = [];
  for (var i = 0; i < g.n; i++) {
    var sn = g.lowShort[i];
    var rank;
    if (sn === q) rank = 0;
    else if (sn.startsWith(q)) rank = 1;
    else if (sn.indexOf(q) >= 0 || g.lowQualified[i].indexOf(q) >= 0) rank = 2;
    else continue;
    hits.push([rank, i]);
  }
  hits.sort(function (a, b) {
    return a[0] - b[0] || cmp(g.ids[a[1]], g.ids[b[1]]);
  });
  return hits.slice(0, SEARCH_LIMIT).map(function (h) {
    return h[1];
  });
}

function group(g, csr, i, endArr) {
  var res = {};
  for (var p = csr.start[i]; p < csr.start[i + 1]; p++) {
    var e = csr.list[p];
    var kn = g.edgeKinds[g.ekind[e]];
    (res[kn] = res[kn] || []).push(g.ids[endArr[e]]);
  }
  Object.keys(res).forEach(function (k) {
    res[k].sort(cmp);
  });
  return res;
}

function details(g, i) {
  if (!Number.isInteger(i) || i < 0 || i >= g.n) {
    throw new RangeError("nœud inconnu : " + i);
  }
  return {
    id: g.ids[i],
    kind: g.nodeKinds[g.kind[i]],
    subkind: g.subkind[i],
    qualified: g.qualified[i],
    file: g.file[i],
    summary: g.summary[i],
    out: group(g, g.out, i, g.dst),
    in: group(g, g.in, i, g.src),
  };
}

if (typeof module !== "undefined") {
  module.exports = {
    readData: readData,
    load: load,
    initialView: initialView,
    search: search,
    details: details,
  };
}
