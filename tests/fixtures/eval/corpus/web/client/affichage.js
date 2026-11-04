// Affichage de la liste des livres dans la page.
export function afficherLivres(conteneur, livres) {
  conteneur.innerHTML = '';
  for (const livre of livres) {
    const ligne = document.createElement('li');
    ligne.textContent = `${livre.titre} (${livre.auteur})`;
    conteneur.appendChild(ligne);
  }
}

export function afficherMessage(zone, texte) {
  zone.textContent = texte;
}
