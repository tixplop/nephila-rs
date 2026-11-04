-- Adherents inscrits a la mediatheque.
CREATE TABLE adherents (
    numero_carte BIGINT PRIMARY KEY,
    nom VARCHAR(100) NOT NULL,
    inscrit_le DATE NOT NULL
);

CREATE INDEX idx_adherents_nom ON adherents (nom);

-- Chaque adherent a une carte unique ; le quota d'emprunts est gere par le code.
CREATE INDEX idx_adherents_inscription ON adherents (inscrit_le);
