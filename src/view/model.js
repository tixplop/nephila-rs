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

var CAP = 300;

// Voisins directs (entrants et sortants) de i, sans doublon ni i lui-même.
function neighborSet(g, i) {
  var set = new Set();
  var p;
  for (p = g.out.start[i]; p < g.out.start[i + 1]; p++) set.add(g.dst[g.out.list[p]]);
  for (p = g.in.start[i]; p < g.in.start[i + 1]; p++) set.add(g.src[g.in.list[p]]);
  set.delete(i);
  return set;
}

function checkNode(g, i) {
  if (!Number.isInteger(i) || i < 0 || i >= g.n) {
    throw new RangeError("nœud inconnu : " + i);
  }
}

function checkCap(cap) {
  if (!Number.isInteger(cap) || cap < 1) throw new RangeError("plafond invalide : " + cap);
}

// Ajoute à `view` les voisins de i, dans la limite du plafond de nœuds affichés
// (plus haut degré d'abord, identifiant à égalité). Retourne { view, hidden }.
function expand(g, view, i, plafond) {
  if (plafond === undefined) plafond = CAP;
  checkNode(g, i);
  checkCap(plafond);
  var next = new Set(view);
  if (next.size < plafond) next.add(i);
  var cands = [];
  neighborSet(g, i).forEach(function (x) {
    if (!next.has(x)) cands.push(x);
  });
  cands.sort(function (a, b) {
    return g.degree[b] - g.degree[a] || cmp(g.ids[a], g.ids[b]);
  });
  var room = next.has(i) ? Math.max(0, plafond - next.size) : 0;
  var take = Math.min(room, cands.length);
  for (var k = 0; k < take; k++) next.add(cands[k]);
  return { view: next, hidden: cands.length - take };
}

// Vue d'un résultat de recherche : le nœud et ses voisins directs, sous le même plafond.
function neighborsView(g, i, plafond) {
  return expand(g, new Set(), i, plafond);
}

// Nœuds et liens à dessiner. filtres : { hiddenNodeKinds, hiddenEdgeKinds } (itérables de noms de genres décochés).
function visible(g, view, filtres) {
  filtres = filtres || {};
  var hn = new Set(filtres.hiddenNodeKinds || []);
  var he = new Set(filtres.hiddenEdgeKinds || []);
  var nodes = [];
  var keep = new Set();
  view.forEach(function (x) {
    if (hn.has(g.nodeKinds[g.kind[x]])) return;
    keep.add(x);
    nodes.push(x);
  });
  nodes.sort(function (a, b) {
    return a - b;
  });
  var edges = [];
  nodes.forEach(function (x) {
    for (var p = g.out.start[x]; p < g.out.start[x + 1]; p++) {
      var e = g.out.list[p];
      if (keep.has(g.dst[e]) && !he.has(g.edgeKinds[g.ekind[e]])) edges.push(e);
    }
  });
  edges.sort(function (a, b) {
    return a - b;
  });
  return { nodes: nodes, edges: edges };
}

if (typeof module !== "undefined") {
  module.exports = {
    readData: readData,
    load: load,
    initialView: initialView,
    search: search,
    details: details,
    expand: expand,
    neighborsView: neighborsView,
    visible: visible,
  };
}
