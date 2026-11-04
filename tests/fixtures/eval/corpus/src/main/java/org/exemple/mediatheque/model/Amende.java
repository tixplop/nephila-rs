package org.exemple.mediatheque.model;

/** Somme due par un adherent pour un retour tardif. */
public class Amende {
    private final Adherent adherent;
    private final long centimes;
    private boolean reglee;

    public Amende(Adherent adherent, long centimes) {
        this.adherent = adherent;
        this.centimes = centimes;
    }

    public Adherent getAdherent() { return adherent; }

    public long getCentimes() { return centimes; }

    public boolean estReglee() { return reglee; }

    public void regler() {
        this.reglee = true;
    }
}
