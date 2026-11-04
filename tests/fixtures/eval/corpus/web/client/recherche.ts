// Recherche de livres cote client.
import { chargerLivres } from './api.js';
import { LivreDto } from './types';

export async function rechercherDisponibles(titre: string): Promise<LivreDto[]> {
  const livres: LivreDto[] = await chargerLivres(titre);
  return livres.filter((l) => l.disponible);
}

export function trierParTitre(livres: LivreDto[]): LivreDto[] {
  return [...livres].sort((a, b) => a.titre.localeCompare(b.titre));
}

export function compterDisponibles(livres: LivreDto[]): number {
  return livres.filter((l) => l.disponible).length;
}
