package org.exemple.mediatheque.model;

/** Un livre du catalogue, identifie par son ISBN. */
public class Livre {
    private final String isbn;
    private final String titre;
    private final String auteur;
    private int exemplairesDisponibles;

    public Livre(String isbn, String titre, String auteur, int exemplaires) {
        this.isbn = isbn;
        this.titre = titre;
        this.auteur = auteur;
        this.exemplairesDisponibles = exemplaires;
    }

    public String getIsbn() { return isbn; }

    public String getTitre() { return titre; }

    public String getAuteur() { return auteur; }

    public boolean estDisponible() {
        return exemplairesDisponibles > 0;
    }

    public void retirerExemplaire() {
        exemplairesDisponibles--;
    }

    public void rendreExemplaire() {
        exemplairesDisponibles++;
    }
}
