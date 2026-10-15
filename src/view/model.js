"use strict";
// Logique de vue (amorce) : lecture du bloc de données, sans DOM.
function readData(text) {
  return JSON.parse(text);
}
if (typeof module !== "undefined") {
  module.exports = { readData: readData };
}
