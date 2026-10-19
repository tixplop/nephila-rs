"use strict";
// Outil de contrôle manuel : node tests/view/synthetic_page.js <page exportée> <n> <sortie>
// Remplace le bloc nephila-data d'une page exportée par randomGraph(n, 3n) (format DEC-028).
// La sortie doit être sous target/view-check/ (jamais sous /tmp).
const fs = require("node:fs");
const path = require("node:path");
const { randomGraph } = require("./helpers.js");

const OPEN = '<script type="application/json" id="nephila-data">';

function escapeJson(v) {
  return JSON.stringify(v).replace(/[<>&\u2028\u2029]/g, (c) => "\\u" + c.charCodeAt(0).toString(16).padStart(4, "0"));
}

function main(argv) {
  if (argv.length !== 3) throw new Error("usage : node tests/view/synthetic_page.js <page exportée> <n> <sortie>");
  const n = Number(argv[1]);
  if (!Number.isInteger(n) || n < 1) throw new Error("n invalide : " + argv[1]);
  const html = fs.readFileSync(argv[0], "utf8");
  const start = html.indexOf(OPEN);
  if (start < 0) throw new Error("bloc nephila-data introuvable");
  const body = start + OPEN.length;
  const end = html.indexOf("</script>", body);
  if (end < 0) throw new Error("fin du bloc introuvable");
  const out = html.slice(0, body) + escapeJson(randomGraph(n, 3 * n)) + html.slice(end);
  fs.mkdirSync(path.dirname(argv[2]), { recursive: true });
  fs.writeFileSync(argv[2], out);
}

try {
  main(process.argv.slice(2));
} catch (e) {
  console.error("erreur : " + e.message);
  process.exit(1);
}
