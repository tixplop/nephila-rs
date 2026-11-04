-- Prets de livres aux adherents.
CREATE TABLE emprunts (
    id BIGINT PRIMARY KEY,
    isbn VARCHAR(13) NOT NULL REFERENCES livres (isbn),
    numero_carte BIGINT NOT NULL REFERENCES adherents (numero_carte),
    emprunte_le DATE NOT NULL,
    rendu_le DATE
);

CREATE VIEW emprunts_en_cours AS
    SELECT id, isbn, numero_carte FROM emprunts WHERE rendu_le IS NULL;

CREATE INDEX idx_emprunts_carte ON emprunts (numero_carte);
