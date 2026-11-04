package org.exemple.mediatheque.outils

import org.exemple.mediatheque.model.Livre
import org.exemple.mediatheque.service.CatalogueService

/** Importe un fichier CSV de livres dans le catalogue. */
class ImportCatalogue {
    CatalogueService catalogue

    int importer(File csv) {
        int total = 0
        csv.eachLine { ligne ->
            def champs = ligne.split(';')
            catalogue.ajouterAuCatalogue(new Livre(champs[0], champs[1], champs[2], 1))
            total++
        }
        return total
    }
}
