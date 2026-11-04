-- Catalogue des livres.
CREATE TABLE livres (
    isbn VARCHAR(13) PRIMARY KEY,
    titre VARCHAR(200) NOT NULL,
    auteur VARCHAR(100) NOT NULL,
    exemplaires INTEGER NOT NULL DEFAULT 1
);

CREATE INDEX idx_livres_titre ON livres (titre);

-- Le nombre d'exemplaires disponibles baisse a chaque pret.
CREATE INDEX idx_livres_auteur ON livres (auteur);
