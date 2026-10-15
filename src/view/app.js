"use strict";
// Rendu (amorce) : en-tête avec le slug et les nombres de nœuds et de liens.
(function () {
  var data = readData(document.getElementById("rkg-data").textContent);
  document.getElementById("header").textContent =
    data.name + " : " + data.nodes.length + " nœuds, " + data.edges.length + " liens";
})();
