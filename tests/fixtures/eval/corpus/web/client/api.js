// Appels du client web vers le serveur de la mediatheque.
const BASE = '/api';

export async function chargerLivres(titre) {
  const reponse = await fetch(`${BASE}/livres?titre=${encodeURIComponent(titre)}`);
  return reponse.json();
}

export async function envoyerEmprunt(carte, isbn) {
  const reponse = await fetch(`${BASE}/emprunts`, {
    method: 'POST',
    body: JSON.stringify({ carte, isbn }),
  });
  return reponse.ok;
}
