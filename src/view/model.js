"use strict";
// Logique de vue pure (sans DOM) : chargement, index, vue initiale, recherche, détails.

var INITIAL_LIMIT = 100;
var SEARCH_LIMIT = 20;
var ALL_LIMIT = 5000;
var LINKS_DELAY_MS = 150;
// Couleurs (design-system : palette.kinds, palette.communities, color.community.other), déclarées ici seulement.
var KIND_PALETTE = ["#4e79a7", "#f28e2b", "#e15759", "#76b7b2", "#59a14f", "#edc948", "#b07aa1", "#ff9da7", "#9c755f", "#7f7f7f"];
var COMMUNITY_PALETTE = ["#36b", "#c60", "#393", "#c33", "#739", "#088", "#855", "#c39"];
var COMMUNITY_OTHER = "#888";
var NOTE_NO_COMMUNITIES = "Communautés non calculées : lancez nephila update, puis exportez de nouveau la page";

function readData(text) {
  return JSON.parse(text);
}

function cmp(a, b) {
  return a < b ? -1 : a > b ? 1 : 0;
}

// Comparateur de nœuds : plus haut degré d'abord, identifiant à égalité.
function byDegree(g) {
  return function (a, b) {
    return g.degree[b] - g.degree[a] || cmp(g.ids[a], g.ids[b]);
  };
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
    community: new Int32Array(n).fill(-1),
    communities: Array.isArray(data.communities) ? data.communities : [],
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
    if (Number.isInteger(r[7]) && r[7] >= 0) g.community[i] = r[7];
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

// Vue initiale (DEC-030) : au plus `limite` modules de plus haut degré (identifiant à égalité), sinon les
// `limite` nœuds de plus haut degré. `hidden` = modules écartés (0 sans module). Retourne { view, hidden }.
function initialView(g, limite) {
  if (limite === undefined) limite = INITIAL_LIMIT;
  if (!Number.isInteger(limite) || limite < 1) throw new RangeError("limite invalide : " + limite);
  var moduleKind = g.nodeKinds.indexOf("module");
  var pool = [];
  var i;
  if (moduleKind >= 0) {
    for (i = 0; i < g.n; i++) if (g.kind[i] === moduleKind) pool.push(i);
  }
  var hasModules = pool.length > 0;
  if (!hasModules) for (i = 0; i < g.n; i++) pool.push(i);
  pool.sort(byDegree(g));
  var kept = pool.slice(0, limite);
  return { view: new Set(kept), hidden: hasModules ? pool.length - kept.length : 0 };
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

// Couleur de remplissage du nœud i : "community" = palette des communautés (numéro < K), sinon couleur neutre ;
// tout autre mode = couleur du genre.
function nodeColor(g, i, mode) {
  if (mode === "community") {
    var c = g.community[i];
    return c >= 0 && c < COMMUNITY_PALETTE.length ? COMMUNITY_PALETTE[c] : COMMUNITY_OTHER;
  }
  return KIND_PALETTE[g.kind[i] % KIND_PALETTE.length];
}

function communityText(g, c) {
  return c + " " + g.communities[c][0] + " (" + g.communities[c][1] + " nœuds)";
}

// Légende du mode communauté : les min(K, n) premières communautés, plus « autres » s'il en reste.
// Retourne [{ color, text }].
function communityLegend(g) {
  var k = Math.min(COMMUNITY_PALETTE.length, g.communities.length);
  var out = [];
  for (var c = 0; c < k; c++) out.push({ color: COMMUNITY_PALETTE[c], text: communityText(g, c) });
  if (g.communities.length > k) {
    var sum = 0;
    for (var r = k; r < g.communities.length; r++) sum += g.communities[r][1];
    out.push({ color: COMMUNITY_OTHER, text: "autres (" + sum + " nœuds)" });
  }
  return out;
}

// État du choix « communauté » : { enabled, note }.
function colorChoice(g) {
  if (g.n === 0) return { enabled: false, note: "" };
  if (g.communities.length === 0) return { enabled: false, note: NOTE_NO_COMMUNITIES };
  return { enabled: true, note: "" };
}

// { number, name, text } de la communauté du nœud i, null sans communauté.
function nodeCommunity(g, i) {
  var c = g.community[i];
  if (c < 0 || c >= g.communities.length) return null;
  return { number: c, name: g.communities[c][0], text: c + " " + g.communities[c][0] };
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
    community: nodeCommunity(g, i),
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
  cands.sort(byDegree(g));
  var room = next.has(i) ? Math.max(0, plafond - next.size) : 0;
  var take = Math.min(room, cands.length);
  for (var k = 0; k < take; k++) next.add(cands[k]);
  return { view: next, hidden: cands.length - take };
}

// Vue d'ensemble : tous les nœuds, ou les `plafond` de plus haut degré (identifiant à égalité) au-delà.
// Sans filtre de genre. Sélection par seuil de degré (comptage), sans tri complet. Retourne { view, hidden }.
function allView(g, plafond) {
  if (plafond === undefined) plafond = ALL_LIMIT;
  checkCap(plafond);
  var i;
  if (g.n <= plafond) {
    var all = new Set();
    for (i = 0; i < g.n; i++) all.add(i);
    return { view: all, hidden: 0 };
  }
  var maxDeg = 0;
  for (i = 0; i < g.n; i++) if (g.degree[i] > maxDeg) maxDeg = g.degree[i];
  var count = new Int32Array(maxDeg + 2);
  for (i = 0; i < g.n; i++) count[g.degree[i]]++;
  // Seuil t : moins de `plafond` nœuds de degré > t, au moins `plafond` de degré >= t.
  var above = 0;
  var t = maxDeg;
  while (t > 0 && above + count[t] < plafond) {
    above += count[t];
    t--;
  }
  var kept = [];
  var ties = [];
  for (i = 0; i < g.n; i++) {
    if (g.degree[i] > t) kept.push(i);
    else if (g.degree[i] === t) ties.push(i);
  }
  ties.sort(byDegree(g));
  var view = new Set(kept);
  for (var k = 0; view.size < plafond && k < ties.length; k++) view.add(ties[k]);
  return { view: view, hidden: g.n - plafond };
}

// Vue d'un résultat de recherche : le nœud et ses voisins directs, sous le même plafond.
function neighborsView(g, i, plafond) {
  return expand(g, new Set(), i, plafond);
}

// Indice du nœud d'identifiant `id`, -1 s'il est inconnu (index construit à la première demande).
function indexOf(g, id) {
  if (!g.idIndex) {
    g.idIndex = new Map();
    for (var i = 0; i < g.n; i++) g.idIndex.set(g.ids[i], i);
  }
  return g.idIndex.has(id) ? g.idIndex.get(id) : -1;
}

// Navigation vers le nœud i : ajouté à la vue s'il manque de la place sous le plafond, sinon la vue est
// recentrée sur i et ses voisins (recentered = true). `hidden` est le compte « non affichés » à montrer
// (0 hors recentrage : l'ancien message n'a plus de sens). Retourne { view, hidden, recentered }.
function navigate(g, view, i, plafond) {
  if (plafond === undefined) plafond = CAP;
  checkNode(g, i);
  checkCap(plafond);
  if (view.has(i)) return { view: new Set(view), hidden: 0, recentered: false };
  if (view.size < plafond) {
    var next = new Set(view);
    next.add(i);
    return { view: next, hidden: 0, recentered: false };
  }
  var r = neighborsView(g, i, plafond);
  return { view: r.view, hidden: r.hidden, recentered: true };
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

// Pas de disposition par forces (répulsion, ressorts, gravité, amortissement, refroidissement).
// p : [{x, y, vx, vy}] ; edges : paires d'indices dans p ; pinned : indice du nœud glissé ou -1.
// La répulsion, nulle au-delà de 300 unités, parcourt une grille de cases de 300 unités. Retourne le nouvel alpha.
var CELL = 300;
var FORWARD = [
  [1, 0],
  [-1, 1],
  [0, 1],
  [1, 1],
];

function repel(p, a, b, alpha) {
  var dx = p[a].x - p[b].x;
  var dy = p[a].y - p[b].y;
  var d2 = dx * dx + dy * dy + 0.01;
  if (d2 > 90000) return;
  var f = (2000 * alpha) / d2;
  var d = Math.sqrt(d2);
  p[a].vx += (dx / d) * f * d;
  p[a].vy += (dy / d) * f * d;
  p[b].vx -= (dx / d) * f * d;
  p[b].vy -= (dy / d) * f * d;
}

function layoutStep(p, edges, alpha, pinned) {
  var n = p.length;
  var a, b, k, dx, dy, d, f;
  var cells = new Map();
  var keys = [];
  var BIG = 1048576;
  for (a = 0; a < n; a++) {
    var cx = Math.floor(p[a].x / CELL);
    var cy = Math.floor(p[a].y / CELL);
    var key = (cx + BIG) * 2097152 + (cy + BIG);
    var cell = cells.get(key);
    if (!cell) {
      cell = { cx: cx, cy: cy, list: [] };
      cells.set(key, cell);
      keys.push(cell);
    }
    cell.list.push(a);
  }
  keys.forEach(function (c) {
    var l = c.list;
    for (a = 0; a < l.length; a++) for (b = a + 1; b < l.length; b++) repel(p, l[a], l[b], alpha);
    for (var q = 0; q < FORWARD.length; q++) {
      var o = cells.get((c.cx + FORWARD[q][0] + BIG) * 2097152 + (c.cy + FORWARD[q][1] + BIG));
      if (!o) continue;
      for (a = 0; a < l.length; a++) for (b = 0; b < o.list.length; b++) repel(p, l[a], o.list[b], alpha);
    }
  });
  for (k = 0; k < edges.length; k++) {
    var s = p[edges[k][0]];
    var t = p[edges[k][1]];
    dx = t.x - s.x;
    dy = t.y - s.y;
    d = Math.sqrt(dx * dx + dy * dy) + 0.01;
    f = (d - 70) * 0.05 * alpha;
    s.vx += (dx / d) * f;
    s.vy += (dy / d) * f;
    t.vx -= (dx / d) * f;
    t.vy -= (dy / d) * f;
  }
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

// État de mouvement (REQ-040) : liens masqués pendant la molette (150 ms) et le déplacement du fond.
function motionState() {
  return { lastWheel: -Infinity, panning: false };
}
function onWheel(m, now) {
  return { lastWheel: now, panning: m.panning };
}
function onPanStart(m) {
  return { lastWheel: m.lastWheel, panning: true };
}
function onPanEnd(m) {
  return { lastWheel: m.lastWheel, panning: false };
}
function linksShown(m, now) {
  return !m.panning && now - m.lastWheel >= LINKS_DELAY_MS;
}
function linksDueAt(m) {
  return m.panning ? null : m.lastWheel + LINKS_DELAY_MS;
}

if (typeof module !== "undefined") {
  module.exports = {
    readData: readData,
    load: load,
    initialView: initialView,
    search: search,
    details: details,
    KIND_PALETTE: KIND_PALETTE,
    COMMUNITY_PALETTE: COMMUNITY_PALETTE,
    COMMUNITY_OTHER: COMMUNITY_OTHER,
    nodeColor: nodeColor,
    communityLegend: communityLegend,
    colorChoice: colorChoice,
    expand: expand,
    neighborsView: neighborsView,
    indexOf: indexOf,
    navigate: navigate,
    visible: visible,
    ALL_LIMIT: ALL_LIMIT,
    allView: allView,
    layoutStep: layoutStep,
    LINKS_DELAY_MS: LINKS_DELAY_MS,
    motionState: motionState,
    onWheel: onWheel,
    onPanStart: onPanStart,
    onPanEnd: onPanEnd,
    linksShown: linksShown,
    linksDueAt: linksDueAt,
  };
}
