// Panier des livres qu'un adherent veut emprunter.
import { envoyerEmprunt } from './api.js';

const livresChoisis = [];

export function ajouterAuPanier(livre) {
  if (!livresChoisis.includes(livre.isbn)) {
    livresChoisis.push(livre.isbn);
  }
}

export function viderPanier() {
  livresChoisis.length = 0;
}

export async function validerPanier(carte) {
  const resultats = [];
  for (const isbn of livresChoisis) {
    resultats.push(await envoyerEmprunt(carte, isbn));
  }
  viderPanier();
  return resultats.every(Boolean);
}
