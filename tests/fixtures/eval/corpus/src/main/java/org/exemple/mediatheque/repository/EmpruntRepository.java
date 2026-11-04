package org.exemple.mediatheque.repository;

import java.time.LocalDate;
import java.util.List;
import org.exemple.mediatheque.model.Adherent;
import org.exemple.mediatheque.model.Emprunt;

/** Acces aux prets stockes dans la table emprunts. */
public interface EmpruntRepository {
    void enregistrer(Emprunt emprunt);

    List<Emprunt> empruntsEnCours(Adherent adherent);

    List<Emprunt> empruntsEnRetard(LocalDate jour);
}
