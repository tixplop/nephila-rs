package org.exemple.mediatheque.recherche

import org.exemple.mediatheque.model.Livre
import org.exemple.mediatheque.service.CatalogueService

/** Recherche de livres par auteur, au-dessus du catalogue. */
class RechercheLivres(private val catalogue: CatalogueService) {
    fun parAuteur(auteur: String): List<Livre> =
        catalogue.rechercher("").filter { it.auteur.contains(auteur, ignoreCase = true) }

    fun premierDisponible(titre: String): Livre? =
        catalogue.rechercher(titre).firstOrNull { it.estDisponible() }
}
