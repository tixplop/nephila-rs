CREATE TABLE meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE files (
    path  TEXT PRIMARY KEY,
    hash  TEXT NOT NULL,
    size  INTEGER NOT NULL,
    mtime INTEGER NOT NULL,
    recorded_at INTEGER NOT NULL DEFAULT 0,
    state TEXT NOT NULL CHECK (state IN ('indexed', 'pending'))
);

-- Enrichissements conservés (DEC-062) : une entrée par chemin, la dernière. Survit à la
-- réinitialisation du graphe ; `payload` = JSON des résumés, concepts et mentions.
CREATE TABLE enrichments (
    path           TEXT PRIMARY KEY,
    content_hash   TEXT NOT NULL,
    model          TEXT NOT NULL,
    prompt_version INTEGER NOT NULL,
    payload        TEXT NOT NULL
);

CREATE TABLE nodes (
    id        TEXT PRIMARY KEY,
    kind      TEXT NOT NULL,
    qualified TEXT NOT NULL,
    short     TEXT NOT NULL,
    file      TEXT REFERENCES files(path) ON DELETE SET NULL,
    subkind   TEXT,
    summary   TEXT,
    -- Fichier ayant posé le résumé (un nœud partagé peut avoir plusieurs déclarants).
    summary_file TEXT,
    -- Nom qualifié aux caractères inversés (DEC-066), écrit par nephila à chaque écriture d'un
    -- nœud : la recherche par suffixe devient un intervalle de préfixe sur l'index ci-dessous.
    qualified_rev TEXT NOT NULL DEFAULT ''
);
CREATE INDEX idx_nodes_short ON nodes(short);
CREATE INDEX idx_nodes_qualified ON nodes(qualified);
CREATE INDEX idx_nodes_qualified_rev ON nodes(qualified_rev);
CREATE INDEX idx_nodes_file ON nodes(file);

-- Déclarations (fichier, nœud) : un nœud existe tant qu'au moins un fichier le déclare (DEC-014).
CREATE TABLE declarations (
    file TEXT NOT NULL REFERENCES files(path) ON DELETE CASCADE,
    node TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    origin TEXT NOT NULL DEFAULT 'parse' CHECK (origin IN ('parse', 'llm')),
    -- Attributs déclarés ; la ligne du nœud en est une projection (DEC-026).
    short TEXT NOT NULL,
    subkind TEXT,
    located INTEGER NOT NULL,
    summary TEXT,
    PRIMARY KEY (file, node)
);
CREATE INDEX idx_declarations_node ON declarations(node);

CREATE TABLE refs (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    source      TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    file        TEXT NOT NULL REFERENCES files(path) ON DELETE CASCADE,
    kind        TEXT NOT NULL,
    target_name TEXT NOT NULL,
    context     TEXT,
    origin      TEXT NOT NULL DEFAULT 'parse' CHECK (origin IN ('parse', 'llm'))
);
CREATE INDEX idx_refs_source ON refs(source);
CREATE INDEX idx_refs_file ON refs(file);
CREATE INDEX idx_refs_target ON refs(target_name);

CREATE TABLE edges (
    source TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    target TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    kind   TEXT NOT NULL,
    PRIMARY KEY (source, target, kind)
);
CREATE INDEX idx_edges_source ON edges(source);
CREATE INDEX idx_edges_target ON edges(target);

CREATE TABLE summary (
    id          INTEGER PRIMARY KEY CHECK (id = 1),
    text        TEXT NOT NULL,
    -- Instant du calcul, en millisecondes depuis l'epoch Unix.
    computed_at INTEGER NOT NULL
);
