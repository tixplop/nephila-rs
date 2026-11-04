package org.exemple.mediatheque.service;

import java.time.LocalDate;
import org.exemple.mediatheque.model.Amende;
import org.exemple.mediatheque.model.Emprunt;
import org.exemple.mediatheque.repository.AmendeRepository;

/** Calcule et facture les amendes de retard. */
public class AmendeService {
    private static final long CENTIMES_PAR_JOUR = 20;

    private final AmendeRepository amendes;

    public AmendeService(AmendeRepository amendes) {
        this.amendes = amendes;
    }

    public long calculerMontant(Emprunt emprunt, LocalDate jour) {
        return emprunt.joursDeRetard(jour) * CENTIMES_PAR_JOUR;
    }

    public void facturerRetard(Emprunt emprunt, LocalDate jour) {
        long montant = calculerMontant(emprunt, jour);
        if (montant > 0) {
            amendes.enregistrer(new Amende(emprunt.getAdherent(), montant));
        }
    }
}
