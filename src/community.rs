//! Communautés du graphe : algorithme de Leiden (Traag, Waltman et van Eck, 2019), maison et déterministe (REQ-146, DEC-119, DEC-121).
//!
//! Fonction pure sur des indices entiers : le résultat ne dépend que du graphe simple (ensemble des paires non orientées
//! distinctes). Un seul fil, un seul flux pseudo-aléatoire (SplitMix64, graine fixe) tiré dans un ordre fixé, sommes de
//! flottants faites dans l'ordre des indices.
//!
//! Le module est consommé par le post-traitement (T-229) : en attendant, ses points d'entrée ne servent qu'aux tests.
#![allow(dead_code)]

use std::collections::VecDeque;

/// Résolution de la modularité (DEC-121).
const RESOLUTION: f64 = 1.0;
/// Aléa du raffinement (valeur `randomness` de graphify).
const THETA: f64 = 0.001;
/// Graine du générateur.
const SEED: u64 = 42;
/// Nombre maximal de niveaux d'agrégation.
const MAX_ITERATIONS: usize = 10;
/// Tolérance sur un gain pour le tenir pour strictement positif.
const EPS: f64 = 1e-10;

/// SplitMix64.
struct SplitMix64(u64);

impl SplitMix64 {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Flottant dans [0, 1).
    fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// Paires normalisées `(min, max)`, triées, dédoublonnées, sans boucle.
fn normalize(n: usize, pairs: &[(u32, u32)]) -> Vec<(u32, u32)> {
    let mut v: Vec<(u32, u32)> = Vec::with_capacity(pairs.len());
    for &(a, b) in pairs {
        assert!(
            (a as usize) < n && (b as usize) < n,
            "paire ({a}, {b}) hors du graphe de {n} nœuds"
        );
        if a != b {
            v.push((a.min(b), a.max(b)));
        }
    }
    v.sort_unstable();
    v.dedup();
    v
}

/// Graphe pondéré en CSR ; `loops[v]` est le poids interne (arêtes absorbées par l'agrégation).
struct Graph {
    n: usize,
    off: Vec<usize>,
    nbr: Vec<u32>,
    w: Vec<f64>,
    /// Force de chaque nœud : somme des poids incidents, boucle comptée deux fois.
    k: Vec<f64>,
}

impl Graph {
    /// `edges` : `(a, b, poids)` avec `a < b`, triées par `(a, b)`, sans doublon.
    fn from_edges(n: usize, edges: &[(u32, u32, f64)], loops: &[f64]) -> Graph {
        let mut deg = vec![0usize; n + 1];
        for &(a, b, _) in edges {
            deg[a as usize] += 1;
            deg[b as usize] += 1;
        }
        let mut off = vec![0usize; n + 1];
        for i in 0..n {
            off[i + 1] = off[i] + deg[i];
        }
        let mut pos = off.clone();
        let mut nbr = vec![0u32; off[n]];
        let mut w = vec![0f64; off[n]];
        for &(a, b, x) in edges {
            nbr[pos[a as usize]] = b;
            w[pos[a as usize]] = x;
            pos[a as usize] += 1;
            nbr[pos[b as usize]] = a;
            w[pos[b as usize]] = x;
            pos[b as usize] += 1;
        }
        let mut k = vec![0f64; n];
        for v in 0..n {
            let mut s = 0.0;
            for x in &w[off[v]..off[v + 1]] {
                s += x;
            }
            k[v] = s + 2.0 * loops[v];
        }
        Graph { n, off, nbr, w, k }
    }

    fn neighbors(&self, v: usize) -> impl Iterator<Item = (usize, f64)> + '_ {
        (self.off[v]..self.off[v + 1]).map(move |i| (self.nbr[i] as usize, self.w[i]))
    }
}

/// Déplacement local rapide par file. Rend vrai si au moins un nœud a changé de communauté.
fn local_move(g: &Graph, part: &mut [u32], m2: f64) -> bool {
    let n = g.n;
    let mut ktot = vec![0f64; n];
    let mut cnt = vec![0u32; n];
    for v in 0..n {
        ktot[part[v] as usize] += g.k[v];
        cnt[part[v] as usize] += 1;
    }
    let mut free: Vec<u32> = (0..n as u32)
        .rev()
        .filter(|&c| cnt[c as usize] == 0)
        .collect();
    let mut queue: VecDeque<u32> = (0..n as u32).collect();
    let mut inq = vec![true; n];
    let mut wc = vec![0f64; n];
    let mut mark = vec![false; n];
    let mut touched: Vec<usize> = Vec::new();
    let mut moved = false;
    while let Some(v) = queue.pop_front() {
        let v = v as usize;
        inq[v] = false;
        let old = part[v] as usize;
        let kv = g.k[v];
        for (u, x) in g.neighbors(v) {
            let c = part[u] as usize;
            if !mark[c] {
                mark[c] = true;
                touched.push(c);
            }
            wc[c] += x;
        }
        ktot[old] -= kv;
        cnt[old] -= 1;
        let stay = wc[old] - RESOLUTION * kv * ktot[old] / m2;
        let mut best = old;
        let mut best_gain = stay + EPS;
        for &c in &touched {
            if c == old {
                continue;
            }
            let gain = wc[c] - RESOLUTION * kv * ktot[c] / m2;
            if gain > best_gain {
                best_gain = gain;
                best = c;
            }
        }
        if cnt[old] > 0 && 0.0 > best_gain {
            while let Some(&c) = free.last() {
                if cnt[c as usize] != 0 {
                    free.pop();
                } else {
                    break;
                }
            }
            if let Some(c) = free.pop() {
                best = c as usize;
            }
        }
        ktot[best] += kv;
        cnt[best] += 1;
        if best != old {
            part[v] = best as u32;
            moved = true;
            if cnt[old] == 0 {
                free.push(old as u32);
            }
            for (u, _) in g.neighbors(v) {
                if part[u] as usize != best && !inq[u] {
                    inq[u] = true;
                    queue.push_back(u as u32);
                }
            }
        }
        for &c in &touched {
            wc[c] = 0.0;
            mark[c] = false;
        }
        touched.clear();
    }
    moved
}

/// Raffinement : dans chaque communauté, fusions aléatoires de sous-communautés bien reliées, à partir des singletons.
fn refine(g: &Graph, part: &[u32], rng: &mut SplitMix64, m2: f64) -> Vec<u32> {
    let n = g.n;
    let mut refc: Vec<u32> = (0..n as u32).collect();
    let mut kref = g.k.clone();
    let mut cnt = vec![1u32; n];
    let mut kc = vec![0f64; n];
    for v in 0..n {
        kc[part[v] as usize] += g.k[v];
    }
    let mut ext = vec![0f64; n];
    for v in 0..n {
        let mut s = 0.0;
        for (u, x) in g.neighbors(v) {
            if part[u] == part[v] {
                s += x;
            }
        }
        ext[v] = s;
    }
    let mut wr = vec![0f64; n];
    let mut mark = vec![false; n];
    let mut touched: Vec<usize> = Vec::new();
    let mut cands: Vec<(usize, f64)> = Vec::new();
    for v in 0..n {
        if refc[v] as usize != v || cnt[v] != 1 {
            continue;
        }
        let c = part[v] as usize;
        let kv = g.k[v];
        if ext[v] < RESOLUTION * kv * (kc[c] - kv) / m2 {
            continue;
        }
        for (u, x) in g.neighbors(v) {
            if part[u] as usize != c {
                continue;
            }
            let r = refc[u] as usize;
            if !mark[r] {
                mark[r] = true;
                touched.push(r);
            }
            wr[r] += x;
        }
        cands.clear();
        for &r in &touched {
            if ext[r] < RESOLUTION * kref[r] * (kc[c] - kref[r]) / m2 {
                continue;
            }
            let delta = wr[r] - RESOLUTION * kv * kref[r] / m2;
            if delta >= 0.0 {
                cands.push((r, delta));
            }
        }
        if !cands.is_empty() {
            let dmax = cands.iter().fold(0.0f64, |a, &(_, d)| a.max(d));
            let stay_w = ((0.0 - dmax) / THETA).exp();
            let total: f64 = stay_w
                + cands
                    .iter()
                    .map(|&(_, d)| ((d - dmax) / THETA).exp())
                    .sum::<f64>();
            let mut r = rng.next_f64() * total;
            let mut chosen = None;
            if r >= stay_w {
                r -= stay_w;
                for &(t, d) in &cands {
                    let p = ((d - dmax) / THETA).exp();
                    chosen = Some(t);
                    if r < p {
                        break;
                    }
                    r -= p;
                }
            }
            if let Some(t) = chosen {
                ext[t] = ext[t] + ext[v] - 2.0 * wr[t];
                kref[t] += kv;
                cnt[t] += 1;
                cnt[v] = 0;
                refc[v] = t as u32;
            }
        }
        for &r in &touched {
            wr[r] = 0.0;
            mark[r] = false;
        }
        touched.clear();
    }
    refc
}

/// Numérote par première apparition ; rend (numéros, nombre de classes).
fn renumber(labels: &[u32]) -> (Vec<u32>, usize) {
    let mut map = vec![u32::MAX; labels.iter().map(|&l| l as usize + 1).max().unwrap_or(0)];
    let mut next = 0u32;
    let out = labels
        .iter()
        .map(|&l| {
            let slot = &mut map[l as usize];
            if *slot == u32::MAX {
                *slot = next;
                next += 1;
            }
            *slot
        })
        .collect();
    (out, next as usize)
}

/// Graphe agrégé selon `agg` (numéros 0..nr).
fn aggregate(g: &Graph, agg: &[u32], nr: usize, loops: &[f64]) -> (Graph, Vec<f64>) {
    let mut new_loops = vec![0f64; nr];
    for v in 0..g.n {
        new_loops[agg[v] as usize] += loops[v];
    }
    let mut edges: Vec<(u32, u32, f64)> = Vec::new();
    for v in 0..g.n {
        for (u, x) in g.neighbors(v) {
            if u <= v {
                continue;
            }
            let (a, b) = (agg[v], agg[u]);
            if a == b {
                new_loops[a as usize] += x;
            } else {
                edges.push((a.min(b), a.max(b), x));
            }
        }
    }
    edges.sort_by_key(|x| (x.0, x.1));
    let mut merged: Vec<(u32, u32, f64)> = Vec::with_capacity(edges.len());
    for e in edges {
        match merged.last_mut() {
            Some(l) if l.0 == e.0 && l.1 == e.1 => l.2 += e.2,
            _ => merged.push(e),
        }
    }
    (Graph::from_edges(nr, &merged, &new_loops), new_loops)
}

/// Coupe chaque communauté en composantes connexes (jamais défavorable à la modularité).
fn split_components(g: &Graph, part: &[u32]) -> Vec<u32> {
    let n = g.n;
    let mut out = vec![u32::MAX; n];
    let mut next = 0u32;
    let mut stack: Vec<usize> = Vec::new();
    for s in 0..n {
        if out[s] != u32::MAX {
            continue;
        }
        out[s] = next;
        stack.push(s);
        while let Some(v) = stack.pop() {
            for (u, _) in g.neighbors(v) {
                if out[u] == u32::MAX && part[u] == part[s] {
                    out[u] = next;
                    stack.push(u);
                }
            }
        }
        next += 1;
    }
    out
}

/// Partition de Leiden : une communauté (numéro brut, sans ordre garanti) par nœud.
///
/// Les paires sont non orientées ; les boucles sont ignorées et les doublons confondus. Les nœuds sans voisin sont
/// rendus en singletons. Chaque communauté rendue induit un sous-graphe connexe.
pub(crate) fn partition(n: usize, pairs: &[(u32, u32)]) -> Vec<u32> {
    let norm = normalize(n, pairs);
    if norm.is_empty() {
        return (0..n as u32).collect();
    }
    let edges: Vec<(u32, u32, f64)> = norm.iter().map(|&(a, b)| (a, b, 1.0)).collect();
    let zero = vec![0f64; n];
    let g0 = Graph::from_edges(n, &edges, &zero);
    let m2 = 2.0 * norm.len() as f64;
    let mut rng = SplitMix64(SEED);

    let mut level: Option<(Graph, Vec<f64>)> = None;
    let mut part: Vec<u32> = (0..n as u32).collect();
    let mut mapping: Vec<u32> = (0..n as u32).collect();
    for _ in 0..MAX_ITERATIONS {
        let (g, loops) = match &level {
            Some((g, l)) => (g, l),
            None => (&g0, &zero),
        };
        let moved = local_move(g, &mut part, m2);
        let (renum, count) = renumber(&part);
        part = renum;
        if !moved || count == g.n {
            break;
        }
        let refined = refine(g, &part, &mut rng, m2);
        let (agg, nr) = renumber(&refined);
        let mut next_part = vec![0u32; nr];
        for v in 0..g.n {
            next_part[agg[v] as usize] = part[v];
        }
        let (next_part, _) = renumber(&next_part);
        let next = aggregate(g, &agg, nr, loops);
        for m in mapping.iter_mut() {
            *m = agg[*m as usize];
        }
        part = next_part;
        level = Some(next);
    }
    let fin: Vec<u32> = mapping.iter().map(|&m| part[m as usize]).collect();
    let (fin, _) = renumber(&fin);
    let (out, _) = renumber(&split_components(&g0, &fin));
    out
}

/// Modularité (résolution 1) d'une partition sur le graphe simple ; 0 si le graphe n'a aucune arête.
pub(crate) fn modularity(n: usize, pairs: &[(u32, u32)], part: &[u32]) -> f64 {
    assert_eq!(part.len(), n, "une communauté par nœud");
    let norm = normalize(n, pairs);
    if norm.is_empty() {
        return 0.0;
    }
    let m = norm.len() as f64;
    let classes = part.iter().map(|&c| c as usize + 1).max().unwrap_or(0);
    let mut inner = vec![0f64; classes];
    let mut tot = vec![0f64; classes];
    for &(a, b) in &norm {
        let (ca, cb) = (part[a as usize] as usize, part[b as usize] as usize);
        tot[ca] += 1.0;
        tot[cb] += 1.0;
        if ca == cb {
            inner[ca] += 1.0;
        }
    }
    let mut q = 0.0;
    for c in 0..classes {
        q += inner[c] / m - (tot[c] / (2.0 * m)).powi(2);
    }
    q
}

/// Part maximale d'une communauté avant seconde passe (DEC-121).
const MAX_FRACTION: f64 = 0.25;
/// Taille au-dessus de laquelle la fraction s'applique seule (plancher du seuil).
const MIN_SPLIT: usize = 10;
/// Cohésion en dessous de laquelle une grande communauté est redécoupée.
const COHESION_SPLIT: f64 = 0.05;
/// Taille minimale d'une communauté pour le test de cohésion.
const COHESION_MIN_SIZE: usize = 50;

/// Une communauté finale : membres triés, pivot (indice d'un membre) et cohésion.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Community {
    pub(crate) members: Vec<u32>,
    pub(crate) pivot: u32,
    pub(crate) cohesion: f64,
}

/// Paires internes de chaque groupe (indices globaux), d'après les paires normalisées.
fn internal_pairs(n: usize, groups: &[Vec<u32>], norm: &[(u32, u32)]) -> Vec<Vec<(u32, u32)>> {
    let mut of = vec![usize::MAX; n];
    for (g, members) in groups.iter().enumerate() {
        for &m in members {
            of[m as usize] = g;
        }
    }
    let mut out = vec![Vec::new(); groups.len()];
    for &(a, b) in norm {
        let g = of[a as usize];
        if g != usize::MAX && g == of[b as usize] {
            out[g].push((a, b));
        }
    }
    out
}

/// Cohésion : arêtes internes sur k(k-1)/2 ; 1,0 pour un seul membre.
fn cohesion_of(k: usize, edges: usize) -> f64 {
    if k <= 1 {
        return 1.0;
    }
    edges as f64 / (k as f64 * (k as f64 - 1.0) / 2.0)
}

/// Seconde passe sur un groupe (membres triés, paires internes) : `Some(parties)` si elle en rend au moins deux.
fn resplit(members: &[u32], pairs: &[(u32, u32)]) -> Option<Vec<Vec<u32>>> {
    let local = |g: u32| members.binary_search(&g).expect("membre du groupe") as u32;
    let lp: Vec<(u32, u32)> = pairs.iter().map(|&(a, b)| (local(a), local(b))).collect();
    let part = partition(members.len(), &lp);
    let count = part.iter().map(|&c| c as usize + 1).max().unwrap_or(0);
    if count < 2 {
        return None;
    }
    let mut parts = vec![Vec::new(); count];
    for (i, &c) in part.iter().enumerate() {
        parts[c as usize].push(members[i]);
    }
    Some(parts)
}

/// Post-traitement d'une partition brute `raw` (une étiquette par nœud) : isolés en singletons, découpe des
/// communautés trop grandes puis peu cohésives, tri. Rend les groupes dans l'ordre des numéros.
fn postprocess(n: usize, pairs: &[(u32, u32)], raw: &[u32]) -> Vec<Vec<u32>> {
    assert_eq!(raw.len(), n, "une étiquette par nœud");
    let norm = normalize(n, pairs);
    let mut has_edge = vec![false; n];
    for &(a, b) in &norm {
        has_edge[a as usize] = true;
        has_edge[b as usize] = true;
    }
    let mut groups: Vec<Vec<u32>> = Vec::new();
    let mut by: std::collections::BTreeMap<u32, Vec<u32>> = Default::default();
    for v in 0..n {
        if has_edge[v] {
            by.entry(raw[v]).or_default().push(v as u32);
        } else {
            groups.push(vec![v as u32]);
        }
    }
    groups.extend(by.into_values());

    // (3) communautés trop grandes
    let max_size = MIN_SPLIT.max((MAX_FRACTION * n as f64).floor() as usize);
    let ip = internal_pairs(n, &groups, &norm);
    let mut next: Vec<Vec<u32>> = Vec::new();
    for (members, p) in groups.into_iter().zip(ip) {
        match (members.len() > max_size)
            .then(|| resplit(&members, &p))
            .flatten()
        {
            Some(parts) => next.extend(parts),
            None => next.push(members),
        }
    }
    // (4) communautés peu cohésives
    let ip = internal_pairs(n, &next, &norm);
    let mut fin: Vec<Vec<u32>> = Vec::new();
    for (members, p) in next.into_iter().zip(ip) {
        let k = members.len();
        let low = k >= COHESION_MIN_SIZE && cohesion_of(k, p.len()) < COHESION_SPLIT;
        match low.then(|| resplit(&members, &p)).flatten() {
            Some(parts) => fin.extend(parts),
            None => fin.push(members),
        }
    }
    // (5) tris
    for g in fin.iter_mut() {
        g.sort_unstable();
    }
    fin.sort_unstable_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
    fin
}

/// Communautés du graphe, dans l'ordre des numéros (indice = numéro).
///
/// Contrat : les indices `0..n` suivent l'ordre d'octets des identifiants `ids` (strictement croissant), ce qui rend la
/// sortie indépendante de l'ordre d'insertion ; l'appelant réindexe en ce sens. `degree` est le degré du résumé
/// (REQ-019). Le nom de chaque communauté est posé par l'appelant.
pub(crate) fn communities(
    n: usize,
    pairs: &[(u32, u32)],
    degree: &[u64],
    ids: &[&str],
) -> Vec<Community> {
    assert_eq!(degree.len(), n, "un degré par nœud");
    assert_eq!(ids.len(), n, "un identifiant par nœud");
    debug_assert!(
        ids.windows(2).all(|w| w[0].as_bytes() < w[1].as_bytes()),
        "les indices doivent suivre l'ordre d'octets des identifiants"
    );
    let norm = normalize(n, pairs);
    // (1)-(2) : partition des seuls nœuds reliés
    let mut compact = vec![u32::MAX; n];
    let mut linked: Vec<u32> = Vec::new();
    for &(a, b) in &norm {
        for v in [a, b] {
            if compact[v as usize] == u32::MAX {
                compact[v as usize] = 0;
                linked.push(v);
            }
        }
    }
    linked.sort_unstable();
    for (i, &v) in linked.iter().enumerate() {
        compact[v as usize] = i as u32;
    }
    let sub: Vec<(u32, u32)> = norm
        .iter()
        .map(|&(a, b)| (compact[a as usize], compact[b as usize]))
        .collect();
    let sub_part = partition(linked.len(), &sub);
    let mut raw = vec![0u32; n];
    for (i, &v) in linked.iter().enumerate() {
        raw[v as usize] = sub_part[i];
    }
    let groups = postprocess(n, pairs, &raw);
    let ip = internal_pairs(n, &groups, &norm);
    groups
        .into_iter()
        .zip(ip)
        .map(|(members, p)| {
            let pivot = members
                .iter()
                .copied()
                .max_by(|&a, &b| {
                    degree[a as usize]
                        .cmp(&degree[b as usize])
                        .then_with(|| b.cmp(&a))
                })
                .expect("communauté non vide");
            let cohesion = cohesion_of(members.len(), p.len());
            Community {
                members,
                pivot,
                cohesion,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeSet, HashSet};

    const KARATE: &[(u32, &[u32])] = &[
        (0, &[1, 2, 3, 4, 5, 6, 7, 8, 10, 11, 12, 13, 17, 19, 21, 31]),
        (1, &[2, 3, 7, 13, 17, 19, 21, 30]),
        (2, &[3, 7, 8, 9, 13, 27, 28, 32]),
        (3, &[7, 12, 13]),
        (4, &[6, 10]),
        (5, &[6, 10, 16]),
        (6, &[16]),
        (8, &[30, 32, 33]),
        (9, &[33]),
        (13, &[33]),
        (14, &[32, 33]),
        (15, &[32, 33]),
        (18, &[32, 33]),
        (19, &[33]),
        (20, &[32, 33]),
        (22, &[32, 33]),
        (23, &[25, 27, 29, 32, 33]),
        (24, &[25, 27, 31]),
        (25, &[31]),
        (26, &[29, 33]),
        (27, &[33]),
        (28, &[31, 33]),
        (29, &[32, 33]),
        (30, &[32, 33]),
        (31, &[32, 33]),
        (32, &[33]),
    ];

    fn karate() -> Vec<(u32, u32)> {
        KARATE
            .iter()
            .flat_map(|&(a, bs)| bs.iter().map(move |&b| (a, b)))
            .collect()
    }

    /// Partition comme ensemble d'ensembles de membres (indépendant de la numérotation).
    fn as_sets(part: &[u32]) -> BTreeSet<Vec<usize>> {
        let mut by: std::collections::BTreeMap<u32, Vec<usize>> = Default::default();
        for (i, &c) in part.iter().enumerate() {
            by.entry(c).or_default().push(i);
        }
        by.into_values().collect()
    }

    /// Chaque communauté induit un sous-graphe connexe (parcours en largeur).
    fn all_connected(n: usize, pairs: &[(u32, u32)], part: &[u32]) -> bool {
        let mut adj = vec![Vec::new(); n];
        for &(a, b) in pairs {
            adj[a as usize].push(b as usize);
            adj[b as usize].push(a as usize);
        }
        let mut seen = vec![false; n];
        let mut visited_comm: HashSet<u32> = HashSet::new();
        for s in 0..n {
            if seen[s] {
                continue;
            }
            if !visited_comm.insert(part[s]) {
                return false; // deuxième composante pour la même communauté
            }
            let mut q = VecDeque::from([s]);
            seen[s] = true;
            while let Some(v) = q.pop_front() {
                for &u in &adj[v] {
                    if !seen[u] && part[u] == part[s] {
                        seen[u] = true;
                        q.push_back(u);
                    }
                }
            }
        }
        true
    }

    struct TestRng(u64);
    impl TestRng {
        fn next(&mut self) -> u64 {
            let mut r = SplitMix64(self.0);
            let v = r.next_u64();
            self.0 = r.0;
            v
        }
        fn below(&mut self, k: usize) -> usize {
            (self.next() % k as u64) as usize
        }
    }

    #[test]
    fn req146_ac2_karate_modularity_connected_deterministic() {
        let pairs = karate();
        assert_eq!(pairs.len(), 78);
        let part = partition(34, &pairs);
        assert_eq!(part.len(), 34);
        let q = modularity(34, &pairs, &part);
        assert!(q >= 0.41, "modularité {q}");
        assert!(all_connected(34, &pairs, &part));
        assert_eq!(part, partition(34, &pairs));
    }

    fn random_graph(rng: &mut TestRng) -> (usize, Vec<(u32, u32)>) {
        let n = rng.below(301);
        let mut pairs = Vec::new();
        if n > 0 {
            let blocks = 1 + rng.below(5);
            let count = rng.below(3 * n + 1);
            for _ in 0..count {
                let a = rng.below(n);
                // le plus souvent dans le même bloc : plusieurs composantes
                let b = if rng.below(10) < 9 {
                    (a % blocks + blocks * rng.below(n / blocks + 1)).min(n - 1)
                } else {
                    rng.below(n)
                };
                let (a, b) = if a.is_multiple_of(blocks) || rng.below(2) == 0 {
                    (a, b)
                } else {
                    (b, a)
                };
                pairs.push((a as u32, b as u32));
                if rng.below(8) == 0 {
                    pairs.push((b as u32, a as u32)); // doublon inversé
                }
                if rng.below(8) == 0 {
                    pairs.push((a as u32, b as u32)); // doublon de même sens
                }
                if rng.below(20) == 0 {
                    pairs.push((a as u32, a as u32)); // boucle
                }
            }
        }
        (n, pairs)
    }

    #[test]
    fn req146_ac3_leiden_properties_on_random_graphs() {
        let mut rng = TestRng(2024);
        for case in 0..200 {
            let (n, pairs) = random_graph(&mut rng);
            let part = partition(n, &pairs);
            assert_eq!(part.len(), n, "cas {case}");
            assert!(
                all_connected(n, &pairs, &part),
                "cas {case} : communauté non connexe"
            );
            let singles: Vec<u32> = (0..n as u32).collect();
            let q = modularity(n, &pairs, &part);
            assert!(q >= modularity(n, &pairs, &singles) - 1e-12, "cas {case}");
            // ordre et orientation des paires permutés
            let mut shuffled = pairs.clone();
            for i in (1..shuffled.len()).rev() {
                shuffled.swap(i, rng.below(i + 1));
            }
            for p in shuffled.iter_mut() {
                if rng.below(2) == 0 {
                    *p = (p.1, p.0);
                }
            }
            assert_eq!(
                as_sets(&part),
                as_sets(&partition(n, &shuffled)),
                "cas {case}"
            );
        }
    }

    #[test]
    fn req146_ac3_edge_cases() {
        assert!(partition(0, &[]).is_empty());
        assert_eq!(modularity(0, &[], &[]), 0.0);
        let p = partition(5, &[]);
        assert_eq!(as_sets(&p).len(), 5);
        // boucles seules : toujours aucune arête
        assert_eq!(as_sets(&partition(3, &[(1, 1)])).len(), 3);
        // deux cliques reliées par un pont
        let mut pairs = Vec::new();
        for i in 0..5u32 {
            for j in i + 1..5 {
                pairs.push((i, j));
                pairs.push((5 + i, 5 + j));
            }
        }
        pairs.push((0, 5));
        let sets = as_sets(&partition(10, &pairs));
        assert_eq!(
            sets,
            BTreeSet::from([vec![0, 1, 2, 3, 4], vec![5, 6, 7, 8, 9]])
        );
    }

    #[test]
    #[should_panic(expected = "hors du graphe")]
    fn req146_ac3_out_of_range_pair_panics() {
        partition(2, &[(0, 2)]);
    }

    #[test]
    #[ignore]
    fn perf_community_partition() {
        let blocks = 200usize;
        let size = 200usize;
        let n = blocks * size;
        let total = 135_000usize;
        let mut rng = TestRng(7);
        let mut pairs = Vec::with_capacity(total);
        for i in 0..total {
            if i % 10 == 9 {
                pairs.push((rng.below(n) as u32, rng.below(n) as u32));
            } else {
                let b = rng.below(blocks) * size;
                pairs.push(((b + rng.below(size)) as u32, (b + rng.below(size)) as u32));
            }
        }
        let t = std::time::Instant::now();
        let part = partition(n, &pairs);
        let d = t.elapsed();
        let q = modularity(n, &pairs, &part);
        eprintln!(
            "perf_community_partition : {d:?}, modularité {q:.4}, {} communautés",
            as_sets(&part).len()
        );
        let (mut internal, mut kept) = (0usize, 0usize);
        for &(a, b) in &pairs {
            if a / size as u32 == b / size as u32 && a != b {
                internal += 1;
                if part[a as usize] == part[b as usize] {
                    kept += 1;
                }
            }
        }
        assert!(
            kept * 10 > internal * 9,
            "{kept} paires internes conservées sur {internal}"
        );
        assert!(d < std::time::Duration::from_secs(1), "durée {d:?}");
    }

    /// Degré du résumé : chaque lien compte aux deux bouts, une boucle deux fois.
    fn degrees(n: usize, pairs: &[(u32, u32)]) -> Vec<u64> {
        let mut d = vec![0u64; n];
        for &(a, b) in pairs {
            d[a as usize] += 1;
            d[b as usize] += 1;
        }
        d
    }

    fn run(n: usize, pairs: &[(u32, u32)]) -> Vec<Community> {
        let names: Vec<String> = (0..n).map(|i| format!("{i:06}")).collect();
        let ids: Vec<&str> = names.iter().map(String::as_str).collect();
        communities(n, pairs, &degrees(n, pairs), &ids)
    }

    fn clique(pairs: &mut Vec<(u32, u32)>, from: u32, size: u32) {
        for i in 0..size {
            for j in i + 1..size {
                pairs.push((from + i, from + j));
            }
        }
    }

    fn path(pairs: &mut Vec<(u32, u32)>, from: u32, size: u32) {
        for i in 0..size - 1 {
            pairs.push((from + i, from + i + 1));
        }
    }

    #[test]
    fn req146_ac1_two_cliques_and_bridge() {
        // a0..a4 = 0..4, b0..b4 = 5..9, pont a0-b0
        let mut pairs = Vec::new();
        clique(&mut pairs, 0, 5);
        clique(&mut pairs, 5, 5);
        pairs.push((0, 5));
        let cs = run(10, &pairs);
        assert_eq!(cs.len(), 2);
        assert_eq!(cs[0].members, vec![0, 1, 2, 3, 4]);
        assert_eq!(cs[0].pivot, 0);
        assert_eq!(cs[0].cohesion, 1.0);
        assert_eq!(cs[1].members, vec![5, 6, 7, 8, 9]);
        assert_eq!(cs[1].pivot, 5);
        assert_eq!(cs[1].cohesion, 1.0);
    }

    #[test]
    fn req146_ac1_pivot_tie_takes_smallest_index() {
        let cs = run(3, &[(0, 1), (1, 2), (0, 2)]);
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0].pivot, 0);
    }

    #[test]
    fn req146_ac4_oversized_community_is_split() {
        let mut pairs = Vec::new();
        clique(&mut pairs, 0, 20);
        clique(&mut pairs, 20, 20);
        pairs.push((0, 20));
        let raw: Vec<u32> = (0..100u32).map(|v| if v < 40 { 0 } else { v }).collect();
        let g = postprocess(100, &pairs, &raw);
        let big: Vec<&Vec<u32>> = g.iter().filter(|m| m.len() > 1).collect();
        assert_eq!(big.len(), 2);
        assert_eq!(big[0], &(0..20).collect::<Vec<u32>>());
        assert_eq!(big[1], &(20..40).collect::<Vec<u32>>());
        assert_eq!(g.len(), 2 + 60);
    }

    #[test]
    fn req146_ac4_community_without_internal_edge_becomes_singletons() {
        // 30 nœuds sans arête entre eux, chacun relié à un nœud extérieur
        let pairs: Vec<(u32, u32)> = (0..30u32).map(|i| (i, 30 + i)).collect();
        let raw: Vec<u32> = (0..100u32).map(|v| if v < 30 { 0 } else { v }).collect();
        let g = postprocess(100, &pairs, &raw);
        assert_eq!(g.len(), 100);
        assert!(g.iter().all(|m| m.len() == 1));
        // 30 nœuds sans aucun lien
        let g = postprocess(100, &[], &vec![0; 100]);
        assert_eq!(g.len(), 100);
    }

    #[test]
    fn req146_ac4_low_cohesion_split_threshold() {
        let mut pairs = Vec::new();
        path(&mut pairs, 0, 20);
        path(&mut pairs, 20, 20);
        path(&mut pairs, 40, 20);
        pairs.push((19, 20));
        pairs.push((39, 40));
        let raw: Vec<u32> = (0..400u32).map(|v| if v < 60 { 0 } else { v }).collect();
        let g = postprocess(400, &pairs, &raw);
        assert!(g.iter().all(|m| m.len() < 60), "60 chemins non découpés");
        assert!(g.iter().filter(|m| m.len() > 1).count() >= 2);

        // 49 nœuds de même forme : gardés
        let mut pairs = Vec::new();
        path(&mut pairs, 0, 16);
        path(&mut pairs, 16, 16);
        path(&mut pairs, 32, 17);
        pairs.push((15, 16));
        pairs.push((31, 32));
        let raw: Vec<u32> = (0..400u32).map(|v| if v < 49 { 0 } else { v }).collect();
        let g = postprocess(400, &pairs, &raw);
        assert_eq!(g[0], (0..49).collect::<Vec<u32>>());
    }

    #[test]
    fn req146_ac5_empty_and_isolated() {
        assert!(run(0, &[]).is_empty());
        let cs = run(3, &[]);
        assert_eq!(cs.len(), 3);
        for (i, c) in cs.iter().enumerate() {
            assert_eq!(c.members, vec![i as u32]);
            assert_eq!(c.pivot, i as u32);
            assert_eq!(c.cohesion, 1.0);
        }
    }

    /// Résultat exprimé en identifiants : (membres, pivot, cohésion) par numéro.
    type Named = Vec<(Vec<String>, String, u64)>;

    fn named(cs: &[Community], ids: &[&str]) -> Named {
        cs.iter()
            .map(|c| {
                (
                    c.members
                        .iter()
                        .map(|&m| ids[m as usize].to_string())
                        .collect(),
                    ids[c.pivot as usize].to_string(),
                    c.cohesion.to_bits(),
                )
            })
            .collect()
    }

    #[test]
    fn req146_ac3_communities_properties_and_permutation() {
        let mut rng = TestRng(77);
        for case in 0..200 {
            let (n, pairs) = random_graph(&mut rng);
            // identifiants textuels aléatoires, distincts
            let mut texts: Vec<String> = (0..n)
                .map(|i| format!("{:x}-{i}", rng.next() % 1_000_000))
                .collect();
            // ordre d'origine : ordre arbitraire des nœuds
            for i in (1..n).rev() {
                texts.swap(i, rng.below(i + 1));
            }
            let deg0 = degrees(n, &pairs);
            // réindexation : tri des identifiants en ordre d'octets
            let reindex = |texts: &[String], pairs: &[(u32, u32)]| {
                let mut order: Vec<usize> = (0..n).collect();
                order.sort_by(|&a, &b| texts[a].as_bytes().cmp(texts[b].as_bytes()));
                let mut new_of = vec![0u32; n];
                for (new, &old) in order.iter().enumerate() {
                    new_of[old] = new as u32;
                }
                let p: Vec<(u32, u32)> = pairs
                    .iter()
                    .map(|&(a, b)| (new_of[a as usize], new_of[b as usize]))
                    .collect();
                let sorted: Vec<String> = order.iter().map(|&o| texts[o].clone()).collect();
                let deg: Vec<u64> = order.iter().map(|&o| deg0[o]).collect();
                (p, sorted, deg)
            };
            let (p1, ids1, d1) = reindex(&texts, &pairs);
            let refs1: Vec<&str> = ids1.iter().map(String::as_str).collect();
            let cs1 = communities(n, &p1, &d1, &refs1);

            // propriétés
            let mut part = vec![u32::MAX; n];
            for (num, c) in cs1.iter().enumerate() {
                assert!(c.members.windows(2).all(|w| w[0] < w[1]), "cas {case}");
                assert!(c.members.contains(&c.pivot), "cas {case}");
                let best = c.members.iter().map(|&m| d1[m as usize]).max().unwrap();
                assert_eq!(d1[c.pivot as usize], best, "cas {case}");
                assert!(
                    c.members
                        .iter()
                        .all(|&m| d1[m as usize] < best || m >= c.pivot),
                    "cas {case} : pivot non minimal"
                );
                for &m in &c.members {
                    assert_eq!(part[m as usize], u32::MAX, "cas {case} : doublon");
                    part[m as usize] = num as u32;
                }
            }
            assert!(part.iter().all(|&c| c != u32::MAX), "cas {case}");
            assert!(cs1.windows(2).all(|w| {
                (std::cmp::Reverse(w[0].members.len()), &w[0].members)
                    < (std::cmp::Reverse(w[1].members.len()), &w[1].members)
            }));
            assert!(all_connected(n, &p1, &part), "cas {case} : non connexe");
            let singles: Vec<u32> = (0..n as u32).collect();
            assert!(
                modularity(n, &p1, &part) >= modularity(n, &p1, &singles) - 1e-12,
                "cas {case}"
            );

            // permutation de l'ordre des nœuds, des paires et de leur orientation
            let mut perm: Vec<usize> = (0..n).collect();
            for i in (1..n).rev() {
                perm.swap(i, rng.below(i + 1));
            }
            let texts2: Vec<String> = perm.iter().map(|&o| texts[o].clone()).collect();
            let mut inv = vec![0u32; n];
            for (new, &old) in perm.iter().enumerate() {
                inv[old] = new as u32;
            }
            let mut pairs2: Vec<(u32, u32)> = pairs
                .iter()
                .map(|&(a, b)| (inv[a as usize], inv[b as usize]))
                .collect();
            for i in (1..pairs2.len()).rev() {
                pairs2.swap(i, rng.below(i + 1));
            }
            for p in pairs2.iter_mut() {
                if rng.below(2) == 0 {
                    *p = (p.1, p.0);
                }
            }
            let deg2 = degrees(n, &pairs2);
            let mut order: Vec<usize> = (0..n).collect();
            order.sort_by(|&a, &b| texts2[a].as_bytes().cmp(texts2[b].as_bytes()));
            let mut new_of = vec![0u32; n];
            for (new, &old) in order.iter().enumerate() {
                new_of[old] = new as u32;
            }
            let p2: Vec<(u32, u32)> = pairs2
                .iter()
                .map(|&(a, b)| (new_of[a as usize], new_of[b as usize]))
                .collect();
            let ids2: Vec<String> = order.iter().map(|&o| texts2[o].clone()).collect();
            let d2: Vec<u64> = order.iter().map(|&o| deg2[o]).collect();
            let refs2: Vec<&str> = ids2.iter().map(String::as_str).collect();
            let cs2 = communities(n, &p2, &d2, &refs2);
            assert_eq!(named(&cs1, &refs1), named(&cs2, &refs2), "cas {case}");
        }
    }
}
