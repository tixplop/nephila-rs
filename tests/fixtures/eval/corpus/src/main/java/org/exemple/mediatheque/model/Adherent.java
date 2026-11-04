package org.exemple.mediatheque.model;

/** Une personne inscrite a la mediatheque. */
public class Adherent {
    public static final int QUOTA_EMPRUNTS = 5;

    private final long numeroCarte;
    private final String nom;
    private int empruntsEnCours;

    public Adherent(long numeroCarte, String nom) {
        this.numeroCarte = numeroCarte;
        this.nom = nom;
    }

    public long getNumeroCarte() { return numeroCarte; }

    public String getNom() { return nom; }

    public boolean peutEmprunter() {
        return empruntsEnCours < QUOTA_EMPRUNTS;
    }

    public void ajouterEmprunt() {
        empruntsEnCours++;
    }

    public void retirerEmprunt() {
        empruntsEnCours--;
    }
}
