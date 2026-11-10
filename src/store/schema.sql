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

-- Enrichissements conservés (DEC-062, DEC-190) : plusieurs entrées par chemin, une par
-- (contenu, modèle, version du prompt, langue). Survit à la réinitialisation du graphe ;
-- `payload` = JSON des résumés, concepts et mentions ; `language` = langue des résumés demandée
-- (DEC-138), comprise dans l'empreinte ; `used_at` = dernier usage (ms depuis l'epoch : écriture
-- ou réapplication ; 0 après migration) ; `served_model` = nom du modèle servi, lu dans les
-- réponses (DEC-208 ; NULL sans réponse du LLM, hors clé ; placée après `used_at` pour que le
-- texte égale celui d'un graphe migré par `ALTER TABLE ... ADD COLUMN`).
CREATE TABLE enrichments (
    path           TEXT NOT NULL,
    content_hash   TEXT NOT NULL,
    model          TEXT NOT NULL,
    prompt_version INTEGER NOT NULL,
    payload        TEXT NOT NULL,
    language       TEXT NOT NULL,
    used_at        INTEGER NOT NULL, served_model TEXT,
    PRIMARY KEY (path, content_hash, model, prompt_version, language)
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
    -- `doc` : phrase de la doc de la déclaration (DEC-180), écrite à chaque écriture de la
    -- structure ; `summary` vaut le résumé LLM s'il y en a un, sinon `doc`. Placée ici pour que
    -- le texte égale celui d'un graphe migré par `ALTER TABLE ... ADD COLUMN`.
    summary TEXT, doc TEXT,
    PRIMARY KEY (file, node)
);
CREATE INDEX idx_declarations_node_file ON declarations(node, file);

-- Contexte d'imports (package, imports) des références d'analyse d'un fichier, stocké une seule
-- fois par fichier (DEC-073) ; les références `llm` n'ont pas de contexte.
CREATE TABLE file_contexts (
    file    TEXT PRIMARY KEY REFERENCES files(path) ON DELETE CASCADE,
    context TEXT NOT NULL
);

-- `id` = rowid : un identifiant nouveau vaut le plus grand présent plus un (ordre d'insertion).
CREATE TABLE refs (
    id          INTEGER PRIMARY KEY,
    source      TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    file        TEXT NOT NULL REFERENCES files(path) ON DELETE CASCADE,
    kind        TEXT NOT NULL,
    target_name TEXT NOT NULL,
    origin      TEXT NOT NULL DEFAULT 'parse' CHECK (origin IN ('parse', 'llm')),
    -- Clé (dernier segment) et tête (avant le premier point) du nom visé, écrites par nephila à
    -- chaque insertion (DEC-076) ; `unresolved` = 1 : comptée non résolue par la dernière passe.
    target_key  TEXT NOT NULL DEFAULT '',
    target_head TEXT,
    unresolved  INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX idx_refs_source ON refs(source);
CREATE INDEX idx_refs_file ON refs(file);
CREATE INDEX idx_refs_key ON refs(target_key);
CREATE INDEX idx_refs_head ON refs(target_head) WHERE target_head IS NOT NULL;
CREATE INDEX idx_refs_unresolved ON refs(unresolved) WHERE unresolved = 1;

CREATE TABLE edges (
    source TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    target TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    kind   TEXT NOT NULL,
    PRIMARY KEY (source, target, kind)
);
CREATE INDEX idx_edges_target ON edges(target);

CREATE TABLE summary (
    id          INTEGER PRIMARY KEY CHECK (id = 1),
    text        TEXT NOT NULL,
    -- Instant du calcul, en millisecondes depuis l'epoch Unix.
    computed_at INTEGER NOT NULL
);

-- Communautés calculées par la dernière passe qui a écrit le résumé (DEC-120) ; sans autre index.
-- `description` (DEC-196) : sur sa propre ligne, virgule en tête, pour que le texte égale celui
-- d'un graphe migré par `ALTER TABLE ... ADD COLUMN`.
CREATE TABLE communities (
    id       INTEGER PRIMARY KEY,
    label    TEXT NOT NULL,
    pivot    TEXT NOT NULL,
    size     INTEGER NOT NULL,
    cohesion REAL NOT NULL
, description TEXT);

-- Sans rowid : la clé primaire est la table elle-même, sans index automatique doublon (DEC-123).
CREATE TABLE node_communities (
    node      TEXT PRIMARY KEY REFERENCES nodes(id) ON DELETE CASCADE,
    community INTEGER NOT NULL
) WITHOUT ROWID;

-- Résultats validés des appels des fichiers envoyés seuls en plusieurs appels, gardés tant que le
-- fichier n'est pas enrichi (DEC-173). `call_key` : empreinte de la requête (REQ-226).
CREATE TABLE enrich_calls (
    path     TEXT NOT NULL,
    call_key TEXT NOT NULL,
    payload  TEXT NOT NULL,
    PRIMARY KEY (path, call_key)
) WITHOUT ROWID;

-- Noms et descriptions de sous-systèmes par empreinte de communauté (DEC-196), conservés par
-- `build` ; les 5 000 lignes les plus récentes (rowid) sont gardées.
CREATE TABLE subsystem_names (
    fingerprint TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    description TEXT NOT NULL
);
