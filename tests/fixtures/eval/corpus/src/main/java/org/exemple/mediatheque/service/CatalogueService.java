package org.exemple.mediatheque.service;

import java.util.List;
import org.exemple.mediatheque.model.Livre;
import org.exemple.mediatheque.repository.LivreRepository;

/** Consultation et enrichissement du catalogue des livres. */
public class CatalogueService {
    private final LivreRepository livres;

    public CatalogueService(LivreRepository livres) {
        this.livres = livres;
    }

    public List<Livre> rechercher(String fragmentTitre) {
        return livres.chercherParTitre(fragmentTitre);
    }

    public void ajouterAuCatalogue(Livre livre) {
        livres.enregistrer(livre);
    }

    public boolean estEmpruntable(String isbn) {
        return livres.trouverParIsbn(isbn).map(Livre::estDisponible).orElse(false);
    }
}
