//! Noms de sous-systèmes (REQ-261, REQ-262, DEC-196) : sélection des lignes envoyées, empreinte
//! d'une communauté et réapplication du cache à chaque passe qui recalcule les communautés.

use crate::error::Result;
use crate::model::Node;
use crate::store::Store;
use std::collections::HashMap;

/// Taille minimale d'une communauté retenue.
pub const MIN_MEMBERS: usize = 10;
/// Nombre minimal de lignes à envoyer pour qu'une communauté soit retenue.
pub const MIN_LINES: usize = 3;
/// Nombre maximal de lignes envoyées.
pub const MAX_LINES: usize = 30;
/// Longueur maximale d'un résumé envoyé, en caractères.
pub const MAX_SUMMARY_CHARS: usize = 200;

/// Version du prompt de la passe ; T-352 la remplace par celle du module du prompt.
pub(crate) const SUBSYSTEM_PROMPT_VERSION: u32 = 1;

/// Lignes `- <identifiant> : <résumé>` d'une communauté : les membres au résumé non vide, par
/// degré décroissant puis identifiant en ordre d'octets, 30 au plus, résumé coupé à 200
/// caractères (AC-261-1).
pub fn select_lines(members: &[Node], degrees: &HashMap<String, u64>) -> Vec<String> {
    let mut kept: Vec<(&Node, String)> = members
        .iter()
        .filter_map(|n| {
            let summary = n.summary.as_deref()?.split_whitespace().collect::<Vec<_>>();
            if summary.is_empty() {
                return None;
            }
            Some((n, summary.join(" ")))
        })
        .collect();
    let degree = |n: &Node| degrees.get(&n.id).copied().unwrap_or(0);
    kept.sort_by(|a, b| {
        degree(b.0)
            .cmp(&degree(a.0))
            .then_with(|| a.0.id.as_bytes().cmp(b.0.id.as_bytes()))
    });
    kept.into_iter()
        .take(MAX_LINES)
        .map(|(n, s)| {
            let cut: String = s.chars().take(MAX_SUMMARY_CHARS).collect();
            format!("- {} : {}", n.id, cut)
        })
        .collect()
}

/// Empreinte d'une communauté (AC-262-1) : BLAKE3 de la version du prompt, de la langue, des
/// identifiants triés de tous les membres et des lignes envoyées, chaque champ précédé de sa
/// longueur pour qu'aucune concaténation ne soit ambiguë.
pub fn fingerprint(
    prompt_version: u32,
    language: &str,
    member_ids: &[String],
    lines: &[String],
) -> String {
    fn field(h: &mut blake3::Hasher, s: &str) {
        h.update(&(s.len() as u64).to_le_bytes());
        h.update(s.as_bytes());
    }
    let mut ids: Vec<&String> = member_ids.iter().collect();
    ids.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    let mut h = blake3::Hasher::new();
    field(&mut h, &prompt_version.to_string());
    field(&mut h, language);
    h.update(&(ids.len() as u64).to_le_bytes());
    for id in ids {
        field(&mut h, id);
    }
    h.update(&(lines.len() as u64).to_le_bytes());
    for l in lines {
        field(&mut h, l);
    }
    h.finalize().to_hex().to_string()
}

/// Réapplique les noms en cache aux communautés retenues dont l'empreinte est connue (REQ-262).
/// Cache vide : aucune lecture de plus.
pub fn reapply(store: &mut Store) -> Result<()> {
    if store.subsystem_names_empty()? {
        return Ok(());
    }
    let language = store.enrich_language()?.unwrap_or_default();
    let mut found = Vec::new();
    for c in store.community_list()? {
        if (c.size as usize) < MIN_MEMBERS {
            continue;
        }
        let members = store.community_members(c.id)?;
        let lines = select_lines(&members, &store.community_degrees(c.id)?);
        if lines.len() < MIN_LINES {
            continue;
        }
        let ids: Vec<String> = members.into_iter().map(|n| n.id).collect();
        let fp = fingerprint(SUBSYSTEM_PROMPT_VERSION, &language, &ids, &lines);
        if let Some((name, description)) = store.subsystem_name(&fp)? {
            found.push((c.id, name, description));
        }
    }
    if found.is_empty() {
        return Ok(());
    }
    store.apply_subsystem_names(&found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{FileRecord, FileState, NodeKind};
    use crate::store::{replace_communities_tx, CommunityRow};

    fn node(i: usize, summary: Option<&str>) -> Node {
        Node::new(
            NodeKind::Type,
            &format!("p.C{i:02}"),
            &format!("C{i:02}"),
            Some("A.java"),
            None,
            summary,
        )
    }

    #[test]
    fn req261_ac1_selects_thirty_lines_by_degree_then_id() {
        let members: Vec<Node> = (0..50)
            .map(|i| node(i, (i < 40).then_some("résumé")))
            .collect();
        let degrees: HashMap<String, u64> = members
            .iter()
            .enumerate()
            .map(|(i, n)| (n.id.clone(), i as u64))
            .collect();
        let lines = select_lines(&members, &degrees);
        assert_eq!(lines.len(), 30);
        assert_eq!(lines[0], format!("- {} : résumé", members[39].id));
        assert_eq!(lines[29], format!("- {} : résumé", members[10].id));
        // Degrés égaux : ordre des identifiants.
        let reversed: Vec<Node> = members.iter().rev().cloned().collect();
        let tie = select_lines(&reversed, &HashMap::new());
        assert_eq!(tie[0], format!("- {} : résumé", members[0].id));
        assert_eq!(tie[29], format!("- {} : résumé", members[29].id));
    }

    #[test]
    fn req261_ac1_skips_empty_summaries_and_cuts_on_characters() {
        let long = "é".repeat(500);
        let members = vec![
            node(0, Some("")),
            node(1, Some("  \n ")),
            node(2, None),
            node(3, Some(&long)),
        ];
        let lines = select_lines(&members, &HashMap::new());
        assert_eq!(lines.len(), 1);
        assert_eq!(
            lines[0],
            format!("- {} : {}", members[3].id, "é".repeat(200))
        );
    }

    #[test]
    fn req262_ac1_fingerprint_depends_on_every_input() {
        let ids: Vec<String> = ["b", "a", "c"].iter().map(|s| s.to_string()).collect();
        let lines: Vec<String> = vec!["- a : x".into(), "- b : y".into()];
        let base = fingerprint(1, "français", &ids, &lines);
        assert_eq!(base, fingerprint(1, "français", &ids, &lines));
        let shuffled: Vec<String> = ["c", "b", "a"].iter().map(|s| s.to_string()).collect();
        assert_eq!(base, fingerprint(1, "français", &shuffled, &lines));
        let mut more = ids.clone();
        more.push("d".into());
        assert_ne!(base, fingerprint(1, "français", &more, &lines));
        let mut changed = lines.clone();
        changed[1] = "- b : z".into();
        assert_ne!(base, fingerprint(1, "français", &ids, &changed));
        assert_ne!(base, fingerprint(1, "English", &ids, &lines));
        assert_ne!(base, fingerprint(2, "français", &ids, &lines));
        // Pas d'ambiguïté de concaténation.
        let a = vec!["ab".to_string(), "c".to_string()];
        let b = vec!["a".to_string(), "bc".to_string()];
        assert_ne!(fingerprint(1, "", &a, &[]), fingerprint(1, "", &b, &[]));
    }

    #[test]
    fn req262_ac6_keeps_the_5000_latest_rows() {
        let dir = tempfile::tempdir().unwrap();
        let mut st = Store::open_or_create(dir.path()).unwrap();
        // 5 000 premières insertions en bloc (une transaction chacune serait trop lente), puis
        // la 5 001e par le store.
        rusqlite::Connection::open(dir.path().join(".nephila/graph.db"))
            .unwrap()
            .execute_batch(
                "WITH RECURSIVE n(i) AS (SELECT 0 UNION ALL SELECT i + 1 FROM n WHERE i < 4999)
                 INSERT INTO subsystem_names(fingerprint, name, description)
                 SELECT 'f' || i, 'n', 'd' FROM n;",
            )
            .unwrap();
        st.put_subsystem_name("f5000", "n", "d").unwrap();
        assert!(st.subsystem_name("f0").unwrap().is_none());
        assert!(st.subsystem_name("f1").unwrap().is_some());
        assert!(st.subsystem_name("f5000").unwrap().is_some());
        let n: i64 = rusqlite::Connection::open(dir.path().join(".nephila/graph.db"))
            .unwrap()
            .query_row("SELECT COUNT(*) FROM subsystem_names", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 5000);
    }

    /// Dépôt à une communauté de 12 membres (résumés pour `with_summaries` d'entre eux).
    fn store_with_community(dir: &std::path::Path, with_summaries: usize) -> (Store, Vec<Node>) {
        let mut st = Store::open_or_create(dir).unwrap();
        st.insert_pending_files(&[FileRecord {
            path: "A.java".into(),
            hash: "h".into(),
            size: 3,
            mtime: 7,
            recorded_at: 0,
            state: FileState::Indexed,
        }])
        .unwrap();
        let nodes: Vec<Node> = (0..12)
            .map(|i| node(i, (i < with_summaries).then_some("fait quelque chose")))
            .collect();
        st.replace_file("A.java", &nodes, &[]).unwrap();
        let conn = rusqlite::Connection::open(dir.join(".nephila/graph.db")).unwrap();
        let row = CommunityRow {
            id: 0,
            label: "C00".into(),
            pivot: nodes[0].id.clone(),
            size: 12,
            cohesion: 0.5,
            description: None,
        };
        let members: Vec<(String, i64)> = nodes.iter().map(|n| (n.id.clone(), 0)).collect();
        replace_communities_tx(&conn, &[row], &members).unwrap();
        (st, nodes)
    }

    fn cached_fingerprint(st: &Store, nodes: &[Node], language: &str) -> String {
        let lines = select_lines(nodes, &st.community_degrees(0).unwrap());
        let ids: Vec<String> = nodes.iter().map(|n| n.id.clone()).collect();
        fingerprint(SUBSYSTEM_PROMPT_VERSION, language, &ids, &lines)
    }

    #[test]
    fn req262_reapply_sets_name_and_description_from_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        let (mut st, nodes) = store_with_community(dir.path(), 5);
        st.set_enrich_language("français").unwrap();
        let fp = cached_fingerprint(&st, &nodes, "français");
        st.put_subsystem_name(&fp, "Paiement", "Gère les paiements.")
            .unwrap();
        reapply(&mut st).unwrap();
        let c = st.community(0).unwrap().unwrap();
        assert_eq!(c.label, "Paiement");
        assert_eq!(c.description.as_deref(), Some("Gère les paiements."));
    }

    #[test]
    fn req262_reapply_keeps_the_pivot_name_when_the_composition_changed() {
        let dir = tempfile::tempdir().unwrap();
        let (mut st, nodes) = store_with_community(dir.path(), 5);
        st.set_enrich_language("français").unwrap();
        // Empreinte d'une autre composition (un membre de moins).
        let lines = select_lines(&nodes, &st.community_degrees(0).unwrap());
        let ids: Vec<String> = nodes[1..].iter().map(|n| n.id.clone()).collect();
        let fp = fingerprint(SUBSYSTEM_PROMPT_VERSION, "français", &ids, &lines);
        st.put_subsystem_name(&fp, "Paiement", "Gère les paiements.")
            .unwrap();
        reapply(&mut st).unwrap();
        let c = st.community(0).unwrap().unwrap();
        assert_eq!(c.label, "C00");
        assert_eq!(c.description, None);
    }

    #[test]
    fn req262_reapply_ignores_a_community_with_fewer_than_three_lines() {
        let dir = tempfile::tempdir().unwrap();
        let (mut st, nodes) = store_with_community(dir.path(), 2);
        let fp = cached_fingerprint(&st, &nodes, "");
        st.put_subsystem_name(&fp, "Paiement", "Gère les paiements.")
            .unwrap();
        reapply(&mut st).unwrap();
        assert_eq!(st.community(0).unwrap().unwrap().label, "C00");
    }

    #[test]
    fn req262_reapply_with_an_empty_cache_reads_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (mut st, _) = store_with_community(dir.path(), 5);
        // Sans les tables des communautés, toute lecture échouerait.
        rusqlite::Connection::open(dir.path().join(".nephila/graph.db"))
            .unwrap()
            .execute_batch("DROP TABLE node_communities; DROP TABLE communities;")
            .unwrap();
        reapply(&mut st).unwrap();
    }
}
