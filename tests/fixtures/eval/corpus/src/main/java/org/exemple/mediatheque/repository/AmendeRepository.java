package org.exemple.mediatheque.repository;

import java.util.List;
import org.exemple.mediatheque.model.Adherent;
import org.exemple.mediatheque.model.Amende;

/** Acces aux amendes stockees dans la table amendes. */
public interface AmendeRepository {
    void enregistrer(Amende amende);

    List<Amende> impayees(Adherent adherent);

    long totalImpaye(Adherent adherent);
}
