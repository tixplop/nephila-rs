"use strict";
const fs = require("node:fs");
const path = require("node:path");

function fixture(name) {
  return JSON.parse(fs.readFileSync(path.join(__dirname, "fixtures", name + ".json"), "utf8"));
}

function loadMaven3() {
  return fixture("maven3");
}

// Graphe sans module de n nœuds `type` ; le nœud j est lié aux (j % 5) suivants.
function noModuleGraph(n) {
  const nodes = [];
  const edges = [];
  for (let i = 0; i < n; i++) {
    const id = "type:p.N" + String(i).padStart(4, "0");
    nodes.push([id, 0, "N" + i, "p.N" + i, null, null, null]);
  }
  for (let j = 0; j < n; j++) {
    for (let k = 1; k <= j % 5; k++) if (j + k < n) edges.push([j, j + k, 0]);
  }
  return { version: 1, name: "synthetic", nodeKinds: ["type"], edgeKinds: ["calls"], nodes, edges };
}

// n modules ; le module j dépend des (j % 4) suivants et contient (j % 3) + 1 types.
function moduleGraph(n) {
  const nodes = [];
  const edges = [];
  for (let i = 0; i < n; i++) {
    const id = "module:g:m" + String(i).padStart(4, "0");
    nodes.push([id, 0, "m" + i, "g:m" + i, null, null, null]);
  }
  for (let j = 0; j < n; j++) {
    for (let k = 1; k <= j % 4; k++) if (j + k < n) edges.push([j, j + k, 0]);
    for (let t = 0; t <= j % 3; t++) {
      const idx = nodes.length;
      nodes.push(["type:g.T" + j + "x" + t, 1, "T" + j + "x" + t, "g.T" + j + "x" + t, null, null, null]);
      edges.push([j, idx, 1]);
    }
  }
  return { version: 1, name: "modules", nodeKinds: ["module", "type"], edgeKinds: ["depends_on", "contains"], nodes, edges };
}

// Nœud 0 au centre, relié à n voisins.
function starGraph(n) {
  const nodes = [["type:p.Center", 0, "Center", "p.Center", null, null, null]];
  const edges = [];
  for (let i = 1; i <= n; i++) {
    const id = "type:p.S" + String(i).padStart(5, "0");
    nodes.push([id, 0, "S" + i, "p.S" + i, null, null, null]);
    edges.push(i % 2 ? [0, i, 0] : [i, 0, 0]);
  }
  return { version: 1, name: "star", nodeKinds: ["type"], edgeKinds: ["calls"], nodes, edges };
}

// Générateur congruentiel linéaire à graine fixe.
function lcg(seed) {
  let s = seed >>> 0;
  return () => {
    s = (Math.imul(s, 1664525) + 1013904223) >>> 0;
    return s / 4294967296;
  };
}

function randomGraph(nodeCount, edgeCount, seed = 42) {
  const rnd = lcg(seed);
  const nodeKinds = ["method", "type"];
  const nodes = new Array(nodeCount);
  for (let i = 0; i < nodeCount; i++) {
    const k = i % 2;
    nodes[i] = [nodeKinds[k] + ":p.C" + i, k, "C" + i, "p.C" + i, null, null, null];
  }
  const edges = new Array(edgeCount);
  for (let e = 0; e < edgeCount; e++) {
    edges[e] = [Math.floor(rnd() * nodeCount), Math.floor(rnd() * nodeCount), Math.floor(rnd() * 2)];
  }
  return { version: 1, name: "random", nodeKinds, edgeKinds: ["calls", "contains"], nodes, edges };
}

module.exports = { loadMaven3, noModuleGraph, moduleGraph, starGraph, randomGraph };
