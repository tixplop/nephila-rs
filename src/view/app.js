"use strict";
// Rendu canvas 2D et interactions. Toute décision de contenu vient de model.js.
(function () {
  var data = readData(document.getElementById("rkg-data").textContent);
  var g = load(data);
  var $ = function (id) {
    return document.getElementById(id);
  };
  var canvas = $("canvas");
  var ctx = canvas.getContext("2d");
  var PALETTE = ["#4e79a7", "#f28e2b", "#e15759", "#76b7b2", "#59a14f", "#edc948", "#b07aa1", "#ff9da7", "#9c755f", "#7f7f7f"];
  var RADIUS = 6;

  var init = initialView(g);
  var view = init.view;
  var hidden = init.hidden;
  var hiddenLabel = "modules";
  var selected = -1;
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
    return PALETTE[g.kind[i] % PALETTE.length];
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
      for (var s = 0; s < 80 && vis.nodes.length > 1; s++) tick();
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

  // Disposition par forces : répulsion, ressorts, gravité vers le centre, refroidissement.
  function tick() {
    var nodes = vis.nodes;
    var n = nodes.length;
    var p = nodes.map(function (i) {
      return pos.get(i);
    });
    var slot = new Map();
    nodes.forEach(function (i, s) {
      slot.set(i, s);
    });
    var a, b, dx, dy, d2, d, f;
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
    vis.edges.forEach(function (e) {
      var s = pos.get(g.src[e]);
      var t = pos.get(g.dst[e]);
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
      if (ptr && ptr.node === nodes[a]) {
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
    if (alpha < 0.005) alpha = 0;
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
    ctx.beginPath();
    vis.edges.forEach(function (e) {
      var s = toScreen(pos.get(g.src[e]));
      var t = toScreen(pos.get(g.dst[e]));
      ctx.moveTo(s.x, s.y);
      ctx.lineTo(t.x, t.y);
    });
    ctx.stroke();
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
  function endDrag(select_) {
    if (!ptr) return;
    var p = ptr;
    ptr = null;
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
    ptr = { node: nodeAt(m.x, m.y), sx: m.x, sy: m.y, moved: false };
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
    var r = expand(g, view, selected);
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
    var r = navigate(g, view, i);
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
        var r = neighborsView(g, i);
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
        sw.style.background = PALETTE[idx % PALETTE.length];
        label.appendChild(sw);
      }
      label.appendChild(document.createTextNode(name));
      container.appendChild(label);
    });
  }

  filterBox($("node-filters"), g.nodeKinds, hiddenNodeKinds, true);
  filterBox($("edge-filters"), g.edgeKinds, hiddenEdgeKinds, false);
  $("search").addEventListener("input", function (ev) {
    renderResults(ev.target.value);
  });
  $("expand").addEventListener("click", extend);
  $("reset").addEventListener("click", function () {
    var r = initialView(g);
    view = r.view;
    hidden = r.hidden;
    hiddenLabel = "modules";
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
