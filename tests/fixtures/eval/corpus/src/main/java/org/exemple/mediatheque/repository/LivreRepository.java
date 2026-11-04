package org.exemple.mediatheque.repository;

import java.util.List;
import java.util.Optional;
import org.exemple.mediatheque.model.Livre;

/** Acces aux livres stockes dans la table livres. */
public interface LivreRepository {
    Optional<Livre> trouverParIsbn(String isbn);

    List<Livre> chercherParTitre(String fragment);

    void enregistrer(Livre livre);
}
