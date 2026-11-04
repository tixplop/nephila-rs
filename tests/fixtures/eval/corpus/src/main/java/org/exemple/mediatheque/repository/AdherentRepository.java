package org.exemple.mediatheque.repository;

import java.util.Optional;
import org.exemple.mediatheque.model.Adherent;

/** Acces aux adherents stockes dans la table adherents. */
public interface AdherentRepository {
    Optional<Adherent> trouverParCarte(long numeroCarte);

    void enregistrer(Adherent adherent);

    boolean existe(long numeroCarte);
}
