//! Noms de sous-systèmes (REQ-261, REQ-262, DEC-196) : sélection des lignes envoyées, empreinte
//! d'une communauté et réapplication du cache à chaque passe qui recalcule les communautés.

use crate::error::{warn, Error, Result, BLOCKED, TRUNCATED};
use crate::index::build::{BuildReport, IndexOptions};
use crate::index::enrich::{Policy, ANNOUNCE};
use crate::llm::{with_format_retries, CallStats, Llm, LlmTotals, SubsystemName};
use crate::model::Node;
use crate::progress::{Meter, Phase};
use crate::store::Store;
use crate::trace;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Mutex};
use std::time::{Duration, Instant};

/// Taille minimale d'une communauté retenue.
pub const MIN_MEMBERS: usize = 10;
/// Nombre minimal de lignes à envoyer pour qu'une communauté soit retenue.
pub const MIN_LINES: usize = 3;
/// Nombre maximal de lignes envoyées.
pub const MAX_LINES: usize = 30;
/// Longueur maximale d'un résumé envoyé, en caractères.
pub const MAX_SUMMARY_CHARS: usize = 200;

pub(crate) use crate::llm::subsystem::SUBSYSTEM_PROMPT_VERSION;

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
            // Déjà appliqué : rien à écrire (AC-261-9).
            if c.label != name || c.description.as_deref() != Some(description.as_str()) {
                found.push((c.id, name, description));
            }
        }
    }
    if found.is_empty() {
        return Ok(());
    }
    store.apply_subsystem_names(&found)
}

/// Bilan de la passe (REQ-261) : communautés nommées par un appel, reprises du cache, laissées
/// (échec ou plafond) et non retenues.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SubsystemsOutcome {
    pub named: usize,
    pub reused: usize,
    pub not_retained: usize,
    /// Communautés retenues restées sans nom à cause d'un échec ou d'un arrêt (ligne `erreur:`).
    pub failed: usize,
    /// Communautés retenues restées sans nom à cause du plafond (A-22).
    pub capped: usize,
    /// Plafond d'appels de la commande, pour le texte d'A-22.
    pub max_calls: Option<usize>,
    /// Totaux des appels de la passe.
    pub totals: LlmTotals,
}

impl SubsystemsOutcome {
    /// Communautés retenues restées sans nom.
    pub fn left(&self) -> usize {
        self.failed + self.capped
    }

    /// Ligne de stdout du bilan (V-002) : seul endroit qui la construit.
    pub fn line(&self) -> String {
        format!(
            "sous-systèmes : {} communautés nommées, {} reprises sans appel, {} laissées, {} non retenues (moins de {MIN_MEMBERS} membres ou de {MIN_LINES} résumés)",
            self.named,
            self.reused,
            self.left(),
            self.not_retained
        )
    }
}

/// Communauté retenue dont le nom n'est pas en cache.
struct Todo {
    id: i64,
    name: String,
    fingerprint: String,
    lines: Vec<String>,
}

/// Cause d'un échec pour A-23 : libellés fixes, jamais le texte reçu.
fn cause(e: &Error) -> String {
    match e {
        Error::Truncated { .. } => format!("{TRUNCATED} ; augmentez max_output_tokens"),
        Error::Refused(_) => BLOCKED.to_string(),
        Error::Rejected { trace, .. } | Error::Redirected { trace, .. } => trace.clone(),
        other => other.to_string(),
    }
}

/// Détail d'une trace `fin` : comme `cause`, sans l'action à faire (design.md : une trace ne
/// reprend jamais l'action).
fn trace_cause(e: &Error) -> String {
    match e {
        Error::Truncated { .. } => TRUNCATED.to_string(),
        Error::Refused(_) => BLOCKED.to_string(),
        other => cause(other),
    }
}

type Reply = (usize, Result<SubsystemName>);

/// Un fil d'envoi : prend les communautés dans l'ordre, un appel (nouveaux essais compris) chacune.
#[allow(clippy::too_many_arguments)]
fn worker(
    todo: &[Todo],
    queue: &Mutex<VecDeque<usize>>,
    policy: &Mutex<Policy>,
    stop: &AtomicBool,
    llm: &dyn Llm,
    retries: u32,
    totals: &Mutex<LlmTotals>,
    tx: mpsc::Sender<Reply>,
) {
    crate::interrupt::block_in_this_thread();
    loop {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        let ticket = {
            let p = policy.lock().unwrap_or_else(|e| e.into_inner());
            if p.capped {
                return;
            }
            p.dispatch()
        };
        let Some(i) = queue.lock().unwrap_or_else(|e| e.into_inner()).pop_front() else {
            return;
        };
        let item = &todo[i];
        let label = format!("communauté {}", item.id);
        let result = with_format_retries(retries, stop, |current, fault| {
            if !policy.lock().unwrap_or_else(|e| e.into_inner()).take(1) {
                return Err(Error::Capped);
            }
            let stats = CallStats::with_word("llm sous-système");
            let traced = trace::enabled();
            if traced {
                let mark = if current >= 2 {
                    format!(" (essai {current}/{})", retries.saturating_add(1))
                } else {
                    String::new()
                };
                let chars: usize = item.lines.iter().map(|l| l.chars().count() + 1).sum();
                trace!(
                    "llm sous-système départ {label} : {} lignes, {chars} car.{mark}",
                    item.lines.len()
                );
            }
            let started = Instant::now();
            let r = llm.name_subsystem(&label, &item.lines, stop, &stats, fault);
            if traced {
                let ms = started.elapsed().as_millis();
                let tokens = crate::index::enrich::tokens_suffix(&stats);
                match &r {
                    Ok(_) => trace!("llm sous-système fin {label} ok {ms} ms{tokens}"),
                    // Essai refusé par le plafond ou sans envoi : ni appel ni fin à tracer.
                    Err(Error::Capped | Error::Deferred | Error::Unsent) => {}
                    Err(e) => trace!(
                        "llm sous-système fin {label} {} {ms} ms : {}{tokens}",
                        trace::classify(e),
                        trace::short(&trace_cause(e), trace::MAX_MESSAGE_CHARS)
                    ),
                }
            }
            totals
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .add(&stats, current >= 2);
            r
        });
        // Une requête refusée laisse la communauté et la passe continue (REQ-261) ; un essai
        // refusé par le plafond n'est pas un échec.
        let counted = match &result {
            Ok(_) => Some(None),
            Err(Error::Capped | Error::Rejected { .. } | Error::Redirected { .. }) => None,
            Err(e) => Some(Some(e)),
        };
        if let Some(error) = counted {
            let halt = policy
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .record(ticket, error);
            if halt {
                stop.store(true, Ordering::SeqCst);
            }
        }
        if tx.send((i, result)).is_err() {
            return;
        }
    }
}

/// Fil de veille : au premier Ctrl-C, demande l'arrêt et annonce A-12.
fn watch_interrupt(policy: &Mutex<Policy>, stop: &AtomicBool, done: &AtomicBool) {
    loop {
        if crate::interrupt::received() >= 1 {
            policy
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .request_stop();
            stop.store(true, Ordering::SeqCst);
            warn(ANNOUNCE);
            return;
        }
        if done.load(Ordering::SeqCst) {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Passe des noms de sous-systèmes (REQ-261, REQ-262) : un appel par communauté retenue dont
/// l'empreinte n'est pas en cache, `concurrency` au plus à la fois, sur ce que le plafond de la
/// commande a laissé à l'enrichissement. Chaque réponse valide est écrite aussitôt ; les noms en
/// cache sont réappliqués à la fin. Rend le bilan ; la politique d'arrêt est reportée sur `report`.
pub(crate) fn run(
    store: &mut Store,
    llm: &dyn Llm,
    opts: &IndexOptions,
    report: &mut BuildReport,
) -> Result<SubsystemsOutcome> {
    let _guard = crate::interrupt::Guard::install();
    let (mut out, todo) = classify(store, opts.max_calls)?;
    trace!(
        "sous-systèmes: début {} communautés à nommer, {} reprises sans appel, {} fils",
        todo.len(),
        out.reused,
        opts.concurrency
    );
    let started = Instant::now();
    if !todo.is_empty() {
        send(store, llm, opts, report, &todo, &mut out)?;
    }
    reapply(store)?;
    trace!(
        "sous-systèmes: fin {} ms, {} nommées, {} laissées",
        started.elapsed().as_millis(),
        out.named,
        out.left()
    );
    if out.totals.calls > 0 {
        trace!("{}", crate::llm::calls_line(&out.totals));
    }
    Ok(out)
}

/// Classe les communautés : non retenues, reprises du cache, ou à nommer par un appel.
fn classify(store: &Store, max_calls: Option<usize>) -> Result<(SubsystemsOutcome, Vec<Todo>)> {
    let language = store.enrich_language()?.unwrap_or_default();
    let mut out = SubsystemsOutcome {
        max_calls,
        ..SubsystemsOutcome::default()
    };
    let mut todo: Vec<Todo> = Vec::new();
    for c in store.community_list()? {
        if (c.size as usize) < MIN_MEMBERS {
            out.not_retained += 1;
            continue;
        }
        let members = store.community_members(c.id)?;
        let lines = select_lines(&members, &store.community_degrees(c.id)?);
        if lines.len() < MIN_LINES {
            out.not_retained += 1;
            continue;
        }
        let ids: Vec<String> = members.into_iter().map(|n| n.id).collect();
        let fingerprint = fingerprint(SUBSYSTEM_PROMPT_VERSION, &language, &ids, &lines);
        if store.subsystem_name(&fingerprint)?.is_some() {
            out.reused += 1;
        } else {
            todo.push(Todo {
                id: c.id,
                name: c.label,
                fingerprint,
                lines,
            });
        }
    }
    Ok((out, todo))
}

/// Bilan sans appel (V-002) : les communautés retenues dont le nom n'est pas en cache sont laissées.
fn tally(store: &Store) -> Result<SubsystemsOutcome> {
    let (mut out, todo) = classify(store, None)?;
    out.capped = todo.len();
    Ok(out)
}

/// Envoie les appels de `todo` et écrit chaque réponse valide aussitôt.
fn send(
    store: &mut Store,
    llm: &dyn Llm,
    opts: &IndexOptions,
    report: &mut BuildReport,
    todo: &[Todo],
    out: &mut SubsystemsOutcome,
) -> Result<()> {
    // Budget unique de la commande : ce que l'enrichissement a laissé.
    let left = opts
        .max_calls
        .map(|m| m.saturating_sub(report.llm.calls as usize));
    let mut initial = Policy::with_cap(left);
    initial.subsystems = true;
    let policy = Mutex::new(initial);
    let queue = Mutex::new((0..todo.len()).collect::<VecDeque<usize>>());
    let totals = Mutex::new(LlmTotals::default());
    let stop = AtomicBool::new(false);
    let done = AtomicBool::new(false);
    let workers = opts.concurrency.clamp(1, todo.len());
    let (tx, rx) = mpsc::channel::<Reply>();
    let mut write_error: Option<Error> = None;
    let mut capped_replies = 0;
    let mut meter = Meter::start(Phase::Subsystems, todo.len());
    std::thread::scope(|scope| {
        for _ in 0..workers {
            let tx = tx.clone();
            let (queue, policy, stop, totals) = (&queue, &policy, &stop, &totals);
            scope.spawn(move || {
                worker(
                    todo,
                    queue,
                    policy,
                    stop,
                    llm,
                    opts.format_retries,
                    totals,
                    tx,
                )
            });
        }
        drop(tx);
        scope.spawn(|| watch_interrupt(&policy, &stop, &done));
        while let Ok((i, result)) = rx.recv() {
            let item = &todo[i];
            // Un appel terminé, réussi ou non, fait avancer la progression ; pas un essai refusé.
            if let Some(m) = meter
                .as_mut()
                .filter(|_| !matches!(&result, Err(Error::Capped | Error::Unsent)))
            {
                m.advance(1);
            }
            match result {
                Ok(named) if write_error.is_none() => {
                    match store.put_subsystem_name(
                        &item.fingerprint,
                        &named.name,
                        &named.description,
                    ) {
                        Ok(()) => out.named += 1,
                        Err(e) => {
                            write_error = Some(e);
                            stop.store(true, Ordering::SeqCst);
                        }
                    }
                }
                Ok(_) => {}
                Err(Error::Capped) => capped_replies += 1,
                // Appel qui n'a rien envoyé : la communauté reste sans nom, sans avertissement.
                Err(Error::Unsent) => out.failed += 1,
                Err(e) => {
                    // La clé refusée est dite par l'erreur finale, sans avertissement par communauté.
                    if !matches!(e, Error::InvalidKey(_)) {
                        warn(&format!(
                            "communauté {} {} : {}",
                            item.id,
                            item.name,
                            cause(&e)
                        ));
                    }
                    out.failed += 1;
                }
            }
        }
        done.store(true, Ordering::SeqCst);
    });
    out.totals = totals.into_inner().unwrap_or_else(|e| e.into_inner());
    let policy = policy.into_inner().unwrap_or_else(|e| e.into_inner());
    report.stop_cause = policy.stop_cause;
    report.refused = policy.refused;
    report.interrupted = policy.interrupted;
    // Communautés jamais envoyées : le plafond les a laissées, sauf si un arrêt a eu lieu avant.
    let unsent = queue.into_inner().unwrap_or_else(|e| e.into_inner()).len();
    if report.stop_cause.is_some() {
        out.failed += unsent + capped_replies;
        out.capped = 0;
    } else {
        out.capped = capped_replies + unsent;
    }
    if let Some(e) = write_error {
        return Err(e);
    }
    Ok(())
}

/// Fin commune de `build` et `update` quand `--subsystems` est donné : pas de LLM, rien à faire
/// ici (A-20 est dit par la commande) ; envoi refusé : rien ; des fichiers restent à enrichir :
/// aucune passe (A-21) ; envoi arrêté sans fichier restant : rien ; sinon la passe.
pub(crate) fn after_enrichment(
    store: &mut Store,
    llm: Option<&dyn Llm>,
    opts: &IndexOptions,
    report: &mut BuildReport,
) -> Result<()> {
    let Some(llm) = llm else {
        return Ok(());
    };
    // Clé ou requête refusée (E-12, E-19, E-20) ou envoi arrêté : aucun appel, la ligne du bilan
    // compte les communautés laissées ; la ligne `erreur:` dit la cause.
    if report.refused.is_some() {
        report.subsystems_tally = Some(tally(store)?);
        return Ok(());
    }
    if report.to_enrich > 0 {
        report.subsystems_skipped = true;
        report.subsystems_tally = Some(tally(store)?);
        return Ok(());
    }
    if report.stop_cause.is_some() || report.interrupted {
        report.subsystems_tally = Some(tally(store)?);
        return Ok(());
    }
    report.subsystems = Some(run(store, llm, opts, report)?);
    Ok(())
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
