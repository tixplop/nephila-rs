"use strict";
// Rendu canvas 2D et interactions. Toute décision de contenu vient de model.js.
(function () {
  var data = readData(document.getElementById("nephila-data").textContent);
  var g = load(data);
  var $ = function (id) {
    return document.getElementById(id);
  };
  var canvas = $("canvas");
  var ctx = canvas.getContext("2d");
  var RADIUS = 6;

  var init = initialView(g);
  var view = init.view;
  var hidden = init.hidden;
  var hiddenLabel = "modules";
  var cap = 300; // plafond de nœuds affichés pour « Étendre » et la navigation (ALL_LIMIT après « Tout étendre »)
  var motion = motionState();
  var linksTimer = null;
  var selected = -1;
  var colorMode = "kind";
  var hiddenNodeKinds = new Set();
  var hiddenEdgeKinds = new Set();
  var pos = new Map();
  var vis = { nodes: [], edges: [] };
  var cam = { k: 1, x: 0, y: 0 };
  var alpha = 1;
  var dirty = true;
  var cw = 1;
  var ch = 1;
  var ptr = null;
  var autoFit = false; // la caméra suit la boîte englobante tant que la disposition bouge
  var keepSelected = false; // le nœud sélectionné est ramené dans la fenêtre s'il en sort
  var MARGIN = 40;

  $("header").textContent = g.name + " : " + g.n + " nœuds, " + g.m + " liens";

  function color(i) {
    return nodeColor(g, i, colorMode);
  }

  function el(tag, text, cls) {
    var e = document.createElement(tag);
    if (text !== undefined) e.textContent = text;
    if (cls) e.className = cls;
    return e;
  }

  // Place les nouveaux nœuds de la vue près de `anchor` (ou du centre) et oublie les autres.
  function syncPositions(anchor) {
    pos.forEach(function (_, k) {
      if (!view.has(k)) pos.delete(k);
    });
    var base = anchor !== undefined && pos.get(anchor);
    var count = 0;
    view.forEach(function (i) {
      if (pos.has(i)) return;
      var a = Math.random() * 2 * Math.PI;
      var r = base ? 30 + Math.random() * 30 : 50 + Math.random() * 150;
      pos.set(i, {
        x: (base ? base.x : 0) + Math.cos(a) * r,
        y: (base ? base.y : 0) + Math.sin(a) * r,
        vx: 0,
        vy: 0,
      });
      count++;
    });
    alpha = Math.max(alpha, count > 0 ? 0.8 : 0.3);
  }

  // opts.fit : cadrer la caméra sur la vue ; opts.keep : ramener le nœud sélectionné dans la fenêtre.
  function refresh(anchor, opts) {
    opts = opts || {};
    syncPositions(anchor);
    dirty = true;
    vis = visible(g, view, { hiddenNodeKinds: hiddenNodeKinds, hiddenEdgeKinds: hiddenEdgeKinds });
    $("message").textContent = hidden > 0 ? hidden + " " + hiddenLabel + " non affichés" : "";
    $("expand").disabled = selected < 0;
    renderDetails();
    alpha = Math.max(alpha, 0.3);
    if (opts.fit) {
      var t0 = performance.now();
    for (var s = 0; s < 80 && vis.nodes.length > 1; s++) {
      tick();
      if (performance.now() - t0 >= 200) break;
    }
      alpha = Math.max(alpha, 0.3);
      autoFit = true;
      keepSelected = false;
      fitCamera();
    } else if (opts.keep && selected >= 0) {
      keepSelected = true;
    }
  }

  // Cadre la caméra sur la boîte englobante des nœuds affichés (marge, zoom borné).
  function fitCamera() {
    if (vis.nodes.length === 0) {
      cam = { k: 1, x: 0, y: 0 };
      dirty = true;
      return;
    }
    var x0 = Infinity;
    var x1 = -Infinity;
    var y0 = Infinity;
    var y1 = -Infinity;
    vis.nodes.forEach(function (i) {
      var q = pos.get(i);
      x0 = Math.min(x0, q.x);
      x1 = Math.max(x1, q.x);
      y0 = Math.min(y0, q.y);
      y1 = Math.max(y1, q.y);
    });
    var k = Math.min((cw - 2 * MARGIN) / Math.max(x1 - x0, 1), (ch - 2 * MARGIN) / Math.max(y1 - y0, 1));
    cam.k = Math.min(2, Math.max(0.05, k));
    cam.x = (-(x0 + x1) / 2) * cam.k;
    cam.y = (-(y0 + y1) / 2) * cam.k;
    dirty = true;
  }

  // Si le nœud sélectionné sort de la fenêtre, la caméra le recentre.
  function bringSelectedIn() {
    if (selected < 0 || !pos.has(selected) || !view.has(selected)) return;
    var q = toScreen(pos.get(selected));
    if (q.x < MARGIN || q.x > cw - MARGIN || q.y < MARGIN || q.y > ch - MARGIN) {
      cam.x = -pos.get(selected).x * cam.k;
      cam.y = -pos.get(selected).y * cam.k;
      dirty = true;
    }
  }

  function userMoved() {
    autoFit = false;
    keepSelected = false;
  }

  // Disposition par forces (layoutStep de model.js) sur les nœuds affichés.
  function tick() {
    var nodes = vis.nodes;
    var slot = new Map();
    var p = nodes.map(function (i, k) {
      slot.set(i, k);
      return pos.get(i);
    });
    var pairs = [];
    vis.edges.forEach(function (e) {
      var s = slot.get(g.src[e]);
      var t = slot.get(g.dst[e]);
      if (s !== undefined && t !== undefined) pairs.push([s, t]);
    });
    var pinned = ptr && ptr.node >= 0 && slot.has(ptr.node) ? slot.get(ptr.node) : -1;
    alpha = layoutStep(p, pairs, alpha, pinned);
  }

  function resize() {
    var r = canvas.getBoundingClientRect();
    var dpr = window.devicePixelRatio || 1;
    cw = Math.max(1, Math.floor(r.width));
    ch = Math.max(1, Math.floor(r.height));
    canvas.width = Math.floor(cw * dpr);
    canvas.height = Math.floor(ch * dpr);
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    dirty = true;
    if (autoFit) fitCamera();
  }

  function toScreen(q) {
    return { x: q.x * cam.k + cam.x + cw / 2, y: q.y * cam.k + cam.y + ch / 2 };
  }
  function toWorld(sx, sy) {
    return { x: (sx - cam.x - cw / 2) / cam.k, y: (sy - cam.y - ch / 2) / cam.k };
  }

  function draw() {
    ctx.clearRect(0, 0, cw, ch);
    ctx.lineWidth = 1;
    ctx.strokeStyle = "#aab";
    if (linksShown(motion, performance.now())) {
      ctx.beginPath();
      vis.edges.forEach(function (e) {
        var s = toScreen(pos.get(g.src[e]));
        var t = toScreen(pos.get(g.dst[e]));
        ctx.moveTo(s.x, s.y);
        ctx.lineTo(t.x, t.y);
      });
      ctx.stroke();
    }
    vis.nodes.forEach(function (i) {
      var q = toScreen(pos.get(i));
      ctx.beginPath();
      ctx.arc(q.x, q.y, RADIUS, 0, 2 * Math.PI);
      ctx.fillStyle = color(i);
      ctx.fill();
      if (i === selected) {
        ctx.lineWidth = 3;
        ctx.strokeStyle = "#000";
        ctx.stroke();
      }
    });
    if (cam.k > 0.6) {
      ctx.fillStyle = "#222";
      ctx.font = "11px sans-serif";
      vis.nodes.forEach(function (i) {
        var q = toScreen(pos.get(i));
        ctx.fillText(g.shortName[i], q.x + RADIUS + 2, q.y + 4);
      });
    }
  }

  function frame() {
    if (alpha > 0) {
      tick();
      if (autoFit && !ptr) fitCamera();
      if (keepSelected && !ptr) bringSelectedIn();
      if (alpha === 0) {
        autoFit = false;
        keepSelected = false;
      }
      draw();
    } else if (dirty) {
      draw();
    }
    dirty = false;
    requestAnimationFrame(frame);
  }

  function nodeAt(sx, sy) {
    for (var s = vis.nodes.length - 1; s >= 0; s--) {
      var i = vis.nodes[s];
      var q = toScreen(pos.get(i));
      var dx = q.x - sx;
      var dy = q.y - sy;
      if (dx * dx + dy * dy <= (RADIUS + 3) * (RADIUS + 3)) return i;
    }
    return -1;
  }

  function mouse(ev) {
    var r = canvas.getBoundingClientRect();
    return { x: ev.clientX - r.left, y: ev.clientY - r.top };
  }

  // Glisser : suivi sur window, avec capture du pointeur ; il se termine au relâchement, à la perte du
  // focus ou si un mouvement arrive sans bouton enfoncé (relâchement manqué hors de la fenêtre).
  // Redessine la vue quand les liens redeviennent visibles (une vue immobile ne se redessine pas seule).
  function scheduleLinks() {
    clearTimeout(linksTimer);
    linksTimer = null;
    var due = linksDueAt(motion);
    if (due === null) return;
    linksTimer = setTimeout(function () {
      linksTimer = null;
      dirty = true;
    }, Math.max(0, due - performance.now()));
  }

  function endDrag(select_) {
    if (!ptr) return;
    var p = ptr;
    ptr = null;
    if (p.panning) {
      motion = onPanEnd(motion);
      dirty = true;
      scheduleLinks();
    }
    if (select_ && !p.moved) select(p.node);
  }

  canvas.addEventListener("pointerdown", function (ev) {
    try {
      canvas.setPointerCapture(ev.pointerId);
    } catch (e) {
      // la capture est facultative : les écouteurs sur window suffisent dans la fenêtre
    }
  });
  canvas.addEventListener("pointercancel", function () {
    endDrag(false);
  });
  window.addEventListener("blur", function () {
    endDrag(false);
  });
  canvas.addEventListener("mousedown", function (ev) {
    if (ev.button !== 0) return;
    var m = mouse(ev);
    ptr = { node: nodeAt(m.x, m.y), sx: m.x, sy: m.y, moved: false, panning: false };
  });
  window.addEventListener("mousemove", function (ev) {
    if (!ptr) return;
    if (ev.buttons === 0) {
      endDrag(false);
      return;
    }
    var m = mouse(ev);
    if (Math.abs(m.x - ptr.sx) + Math.abs(m.y - ptr.sy) > 3) ptr.moved = true;
    if (!ptr.moved) return;
    userMoved();
    dirty = true;
    if (ptr.node < 0 && !ptr.panning) {
      ptr.panning = true;
      motion = onPanStart(motion);
      clearTimeout(linksTimer);
      linksTimer = null;
    }
    if (ptr.node >= 0) {
      var w = toWorld(m.x, m.y);
      var q = pos.get(ptr.node);
      q.x = w.x;
      q.y = w.y;
      alpha = Math.max(alpha, 0.3);
    } else {
      cam.x += m.x - ptr.sx;
      cam.y += m.y - ptr.sy;
      ptr.sx = m.x;
      ptr.sy = m.y;
    }
  });
  window.addEventListener("mouseup", function () {
    endDrag(true);
  });
  canvas.addEventListener("dblclick", function (ev) {
    var m = mouse(ev);
    var i = nodeAt(m.x, m.y);
    if (i >= 0) {
      select(i);
      extend();
    }
  });
  canvas.addEventListener(
    "wheel",
    function (ev) {
      ev.preventDefault();
      userMoved();
      dirty = true;
      motion = onWheel(motion, performance.now());
      scheduleLinks();
      var m = mouse(ev);
      var before = toWorld(m.x, m.y);
      cam.k = Math.min(8, Math.max(0.02, cam.k * (ev.deltaY < 0 ? 1.15 : 1 / 1.15)));
      cam.x = m.x - cw / 2 - before.x * cam.k;
      cam.y = m.y - ch / 2 - before.y * cam.k;
    },
    { passive: false }
  );

  function select(i) {
    selected = i;
    dirty = true;
    $("expand").disabled = i < 0;
    renderDetails();
  }

  function extend() {
    if (selected < 0) return;
    var r = expand(g, view, selected, cap);
    view = r.view;
    hidden = r.hidden;
    hiddenLabel = "voisins";
    refresh(selected, { keep: true });
  }

  // Navigation : le nœud visé est ajouté à la vue (ou la vue est recentrée) et sélectionné.
  function navigateTo(id) {
    var i = indexOf(g, id);
    if (i < 0) return;
    var anchor = selected;
    var r = navigate(g, view, i, cap);
    view = r.view;
    hidden = r.hidden;
    hiddenLabel = "voisins";
    selected = i;
    if (r.recentered) {
      pos.clear();
      refresh(undefined, { fit: true });
    } else {
      refresh(anchor >= 0 ? anchor : undefined, { keep: true });
    }
  }

  function renderDetails() {
    var box = $("details");
    box.textContent = "";
    if (selected < 0) return;
    var d = details(g, selected);
    box.appendChild(el("h2", d.id));
    [
      ["Genre", d.kind],
      ["Sous-genre", d.subkind],
      ["Nom qualifié", d.qualified],
      ["Fichier", d.file],
      ["Communauté", d.community && d.community.text],
      ["Résumé", d.summary],
    ].forEach(function (f) {
      if (f[1] === null || f[1] === undefined || f[1] === "") return;
      var p = el("div", undefined, "field");
      p.appendChild(el("b", f[0] + " : "));
      p.appendChild(document.createTextNode(f[1]));
      box.appendChild(p);
    });
    [
      ["Liens sortants", d.out],
      ["Liens entrants", d.in],
    ].forEach(function (s) {
      var kinds = Object.keys(s[1]);
      if (kinds.length === 0) return;
      box.appendChild(el("h2", s[0]));
      kinds.forEach(function (k) {
        box.appendChild(el("b", k));
        s[1][k].forEach(function (id) {
          var a = el("a", id, "link");
          a.addEventListener("click", function () {
            navigateTo(id);
          });
          box.appendChild(a);
        });
      });
    });
  }

  function renderResults(text) {
    var ul = $("results");
    ul.textContent = "";
    search(g, text).forEach(function (i) {
      var li = el("li", g.ids[i]);
      li.title = g.ids[i];
      li.addEventListener("click", function () {
        cap = 300;
        var r = neighborsView(g, i, cap);
        view = r.view;
        hidden = r.hidden;
        hiddenLabel = "voisins";
        selected = i;
        pos.clear();
        refresh(undefined, { fit: true });
      });
      ul.appendChild(li);
    });
  }

  function filterBox(container, kinds, hiddenSet, colored) {
    kinds.forEach(function (name, idx) {
      var label = el("label", undefined, "filter");
      var cb = document.createElement("input");
      cb.type = "checkbox";
      cb.checked = true;
      cb.addEventListener("change", function () {
        if (cb.checked) hiddenSet.delete(name);
        else hiddenSet.add(name);
        refresh(undefined);
      });
      label.appendChild(cb);
      if (colored) {
        var sw = el("span", undefined, "swatch");
        sw.style.background = KIND_PALETTE[idx % KIND_PALETTE.length];
        label.appendChild(sw);
      }
      label.appendChild(document.createTextNode(name));
      container.appendChild(label);
    });
  }

  function renderLegend() {
    var ul = $("legend");
    ul.textContent = "";
    if (colorMode === "community") {
      communityLegend(g).forEach(function (entry) {
        var li = el("li");
        var sw = el("span", undefined, "swatch");
        sw.style.background = entry.color;
        li.appendChild(sw);
        li.appendChild(document.createTextNode(entry.text));
        li.title = entry.text;
        ul.appendChild(li);
      });
    }
    ul.hidden = colorMode !== "community";
    $("node-filters").classList.toggle("by-community", colorMode === "community");
  }

  var choice = colorChoice(g);
  $("color-community").disabled = !choice.enabled;
  $("color-note").textContent = choice.note;
  ["color-kind", "color-community"].forEach(function (id) {
    $(id).addEventListener("change", function () {
      if (!$(id).checked) return;
      colorMode = $(id).value;
      renderLegend();
      dirty = true;
    });
  });

  filterBox($("node-filters"), g.nodeKinds, hiddenNodeKinds, true);
  filterBox($("edge-filters"), g.edgeKinds, hiddenEdgeKinds, false);
  $("search").addEventListener("input", function (ev) {
    renderResults(ev.target.value);
  });
  $("expand").addEventListener("click", extend);
  $("expand-all").addEventListener("click", function () {
    var r = allView(g);
    view = r.view;
    hidden = r.hidden;
    hiddenLabel = "nœuds";
    cap = ALL_LIMIT;
    if (selected >= 0 && !view.has(selected)) selected = -1;
    refresh(undefined, { fit: true });
  });
  $("reset").addEventListener("click", function () {
    var r = initialView(g);
    view = r.view;
    hidden = r.hidden;
    hiddenLabel = "modules";
    cap = 300;
    selected = -1;
    pos.clear();
    cam = { k: 1, x: 0, y: 0 };
    refresh(undefined, { fit: true });
  });
  window.addEventListener("resize", resize);

  resize();
  refresh(undefined, { fit: true });
  requestAnimationFrame(frame);
})();
