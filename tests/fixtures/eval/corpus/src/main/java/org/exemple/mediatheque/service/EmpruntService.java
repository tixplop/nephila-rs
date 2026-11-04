package org.exemple.mediatheque.service;

import java.time.LocalDate;
import org.exemple.mediatheque.model.Adherent;
import org.exemple.mediatheque.model.Emprunt;
import org.exemple.mediatheque.model.Livre;
import org.exemple.mediatheque.repository.AdherentRepository;
import org.exemple.mediatheque.repository.EmpruntRepository;
import org.exemple.mediatheque.repository.LivreRepository;

/** Enregistre les prets et les retours de livres. */
public class EmpruntService {
    private final LivreRepository livres;
    private final AdherentRepository adherents;
    private final EmpruntRepository emprunts;
    private final AmendeService amendes;

    public EmpruntService(LivreRepository livres, AdherentRepository adherents,
                          EmpruntRepository emprunts, AmendeService amendes) {
        this.livres = livres;
        this.adherents = adherents;
        this.emprunts = emprunts;
        this.amendes = amendes;
    }

    public Emprunt emprunter(long numeroCarte, String isbn) {
        Adherent adherent = adherents.trouverParCarte(numeroCarte).orElseThrow();
        Livre livre = livres.trouverParIsbn(isbn).orElseThrow();
        if (!adherent.peutEmprunter() || !livre.estDisponible()) {
            throw new IllegalStateException("pret impossible");
        }
        Emprunt emprunt = new Emprunt(livre, adherent, LocalDate.now());
        livre.retirerExemplaire();
        adherent.ajouterEmprunt();
        emprunts.enregistrer(emprunt);
        return emprunt;
    }

    public void rendre(Emprunt emprunt) {
        LocalDate aujourdhui = LocalDate.now();
        emprunt.cloturer(aujourdhui);
        emprunt.getLivre().rendreExemplaire();
        emprunt.getAdherent().retirerEmprunt();
        amendes.facturerRetard(emprunt, aujourdhui);
    }
}
