-- Amendes de retard dues par les adherents.
CREATE TABLE amendes (
    id BIGINT PRIMARY KEY,
    numero_carte BIGINT NOT NULL REFERENCES adherents (numero_carte),
    centimes BIGINT NOT NULL,
    reglee BOOLEAN NOT NULL DEFAULT FALSE
);

CREATE INDEX idx_amendes_carte ON amendes (numero_carte);

-- Les amendes impayees bloquent les nouveaux prets.
CREATE VIEW amendes_impayees AS
    SELECT id, numero_carte, centimes FROM amendes WHERE reglee = FALSE;
