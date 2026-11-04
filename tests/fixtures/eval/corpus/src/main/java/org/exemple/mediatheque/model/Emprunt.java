package org.exemple.mediatheque.model;

import java.time.LocalDate;
import java.time.temporal.ChronoUnit;

/** Le pret d'un livre a un adherent, avec sa date de retour prevue. */
public class Emprunt {
    public static final int DUREE_JOURS = 21;

    private final Livre livre;
    private final Adherent adherent;
    private final LocalDate dateEmprunt;
    private LocalDate dateRetour;

    public Emprunt(Livre livre, Adherent adherent, LocalDate dateEmprunt) {
        this.livre = livre;
        this.adherent = adherent;
        this.dateEmprunt = dateEmprunt;
    }

    public Livre getLivre() { return livre; }

    public Adherent getAdherent() { return adherent; }

    public LocalDate dateRetourPrevue() {
        return dateEmprunt.plusDays(DUREE_JOURS);
    }

    public void cloturer(LocalDate jour) {
        this.dateRetour = jour;
    }

    public long joursDeRetard(LocalDate jour) {
        return Math.max(0, ChronoUnit.DAYS.between(dateRetourPrevue(), jour));
    }
}
