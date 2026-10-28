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
}
