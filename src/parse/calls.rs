//! Heuristiques d'appels indépendantes du langage (DEC-021) : le type du receveur n'est
//! cherché que dans le fichier. Un receveur de type inconnu ne produit aucune référence.

use std::collections::{HashMap, HashSet};

use crate::model::{node_id, EdgeKind, NodeKind};

use super::jvm::{clean_type_name, Builder};

/// Types locaux d'une méthode : paramètres et variables locales (nom -> type sans génériques).
#[derive(Debug, Default, Clone)]
pub struct Locals(HashMap<String, String>);

impl Locals {
    pub fn new() -> Self {
        Self::default()
    }

    /// Déclare `name` de type `ty` tel qu'écrit dans le code. Un type absent, `var`, `val`,
    /// `def` ou qui n'est pas un nom de type (tableau, primitive en minuscule...) est ignoré,
    /// et masque alors un éventuel champ homonyme.
    pub fn declare(&mut self, name: &str, ty: &str) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        match type_name(ty) {
            Some(t) => self.0.insert(name.to_string(), t),
            None => self.0.insert(name.to_string(), String::new()),
        };
    }

    fn get(&self, name: &str) -> Option<&String> {
        self.0.get(name)
    }
}

/// Nom de type exploitable, génériques retirés, ou `None` (inconnu, `var`, tableau, primitive).
pub fn type_name(raw: &str) -> Option<String> {
    let t = clean_type_name(raw);
    let t = t.trim_end_matches('?').to_string();
    let valid = !t.is_empty()
        && t.split('.').all(|seg| {
            let mut chars = seg.chars();
            chars
                .next()
                .is_some_and(|c| c.is_alphabetic() || c == '_' || c == '$')
                && chars.all(|c| c.is_alphanumeric() || c == '_' || c == '$')
        });
    let reserved = matches!(t.as_str(), "var" | "val" | "def" | "auto");
    let primitive = t.chars().next().is_some_and(char::is_lowercase) && !t.contains('.');
    (valid && !reserved && !primitive).then_some(t)
}

/// Receveur d'un appel, tel que reconnu par l'extracteur du langage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Receiver {
    /// `m()`.
    Implicit,
    /// `this.m()`.
    This,
    /// `x.m()` où `x` est un simple identifiant.
    Name(String),
    /// `this.x.m()` : champ du type englobant uniquement.
    ThisField(String),
    /// Chaîne d'appels, expression quelconque : aucune référence.
    Unknown,
}

#[derive(Debug)]
struct Pending {
    /// Nom qualifié de la méthode appelante.
    method: String,
    /// Types englobants (du plus externe au plus interne) à l'endroit de l'appel.
    types: Vec<String>,
    /// Type du receveur quand il est connu par une variable locale.
    typed: Option<String>,
    receiver: Receiver,
    name: String,
}

/// Collecte les champs, méthodes et appels d'un fichier ; les références sont émises à la fin
/// (un champ ou une méthode peut être déclaré après son usage).
#[derive(Debug, Default)]
pub struct Calls {
    fields: HashMap<(String, String), String>,
    methods: HashSet<(String, String)>,
    pending: Vec<Pending>,
}

impl Calls {
    pub fn new() -> Self {
        Self::default()
    }

    /// Champ ou propriété `name` du type `type_q` (nom qualifié), de type `ty`.
    pub fn declare_field(&mut self, type_q: &str, name: &str, ty: &str) {
        if let Some(t) = type_name(ty) {
            self.fields
                .insert((type_q.to_string(), name.trim().to_string()), t);
        }
    }

    /// Méthode `name` déclarée par le type `type_q`.
    pub fn declare_method(&mut self, type_q: &str, name: &str) {
        self.methods.insert((type_q.to_string(), name.to_string()));
    }

    /// Enregistre un appel fait dans `method` (nom qualifié), `types` étant la pile des types
    /// englobants.
    pub fn record(
        &mut self,
        method: &str,
        types: &[String],
        locals: &Locals,
        receiver: Receiver,
        name: &str,
    ) {
        let mut typed = None;
        if let Receiver::Name(n) = &receiver {
            if let Some(t) = locals.get(n) {
                // Variable locale : son type (vide = inconnu) masque tout champ.
                typed = Some(t.clone());
            }
        }
        self.pending.push(Pending {
            method: method.to_string(),
            types: types.to_vec(),
            typed,
            receiver,
            name: name.to_string(),
        });
    }

    fn field_type(&self, types: &[String], name: &str) -> Option<&String> {
        types
            .iter()
            .rev()
            .find_map(|t| self.fields.get(&(t.clone(), name.to_string())))
    }

    fn declaring_type<'a>(&self, types: &'a [String], name: &str) -> Option<&'a String> {
        types
            .iter()
            .rev()
            .find(|t| self.methods.contains(&((*t).clone(), name.to_string())))
    }

    /// Cible `Type.m` de l'appel, ou `None` si le receveur est de type inconnu.
    fn target(&self, p: &Pending) -> Option<String> {
        let name = &p.name;
        match &p.receiver {
            Receiver::Unknown => None,
            Receiver::Implicit | Receiver::This => {
                Some(match self.declaring_type(&p.types, name) {
                    Some(t) => format!("{t}.{name}"),
                    None => name.clone(),
                })
            }
            Receiver::ThisField(f) => {
                let t = self.field_type(&p.types, f)?;
                Some(format!("{t}.{name}"))
            }
            Receiver::Name(n) => {
                let ty = match &p.typed {
                    Some(t) if t.is_empty() => return None,
                    Some(t) => Some(t.clone()),
                    None => self.field_type(&p.types, n).cloned(),
                };
                match ty {
                    Some(t) => Some(format!("{t}.{name}")),
                    None if n.chars().next().is_some_and(char::is_uppercase) => {
                        Some(format!("{n}.{name}"))
                    }
                    None => None,
                }
            }
        }
    }

    /// Ajoute au `Builder` les références `calls` (une par couple méthode appelante / cible).
    pub fn finish(self, b: &mut Builder) {
        for p in &self.pending {
            if let Some(target) = self.target(p) {
                b.add_ref(
                    &node_id(NodeKind::Method, &p.method),
                    EdgeKind::Calls,
                    &target,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(f: impl FnOnce(&mut Calls)) -> Vec<(String, String)> {
        let mut c = Calls::new();
        f(&mut c);
        let mut b = Builder::new();
        c.finish(&mut b);
        b.finish()
            .references
            .into_iter()
            .map(|r| (r.source, r.target))
            .collect()
    }

    fn ty() -> Vec<String> {
        vec!["p.S".to_string()]
    }

    #[test]
    fn type_name_nettoie_et_refuse() {
        assert_eq!(type_name("List<Order>").as_deref(), Some("List"));
        assert_eq!(type_name(" a.B<C> ").as_deref(), Some("a.B"));
        assert_eq!(type_name("var"), None);
        assert_eq!(type_name("int"), None);
        assert_eq!(type_name("String[]"), None);
        assert_eq!(type_name(""), None);
    }

    #[test]
    fn receveur_type_par_champ_ou_local() {
        let refs = run(|c| {
            c.declare_field("p.S", "repo", "Repo<X>");
            let mut l = Locals::new();
            l.declare("o", "Order");
            c.record("p.S.m", &ty(), &l, Receiver::Name("repo".into()), "save");
            c.record("p.S.m", &ty(), &l, Receiver::Name("o".into()), "total");
            c.record("p.S.m", &ty(), &l, Receiver::ThisField("repo".into()), "x");
        });
        let t: Vec<&str> = refs.iter().map(|r| r.1.as_str()).collect();
        assert_eq!(t, ["Repo.save", "Order.total", "Repo.x"]);
        assert!(refs.iter().all(|r| r.0 == "method:p.S.m"));
    }

    #[test]
    fn local_masque_le_champ_et_var_inconnu_ne_donne_rien() {
        let refs = run(|c| {
            c.declare_field("p.S", "x", "Field");
            let mut l = Locals::new();
            l.declare("x", "var");
            c.record("p.S.m", &ty(), &l, Receiver::Name("x".into()), "go");
        });
        assert!(refs.is_empty());
    }

    #[test]
    fn statique_majuscule_et_inconnu_minuscule() {
        let refs = run(|c| {
            let l = Locals::new();
            c.record("p.S.m", &ty(), &l, Receiver::Name("Util".into()), "f");
            c.record("p.S.m", &ty(), &l, Receiver::Name("util".into()), "f");
            c.record("p.S.m", &ty(), &l, Receiver::Unknown, "f");
        });
        assert_eq!(refs, [("method:p.S.m".to_string(), "Util.f".to_string())]);
    }

    #[test]
    fn this_et_appel_implicite() {
        let refs = run(|c| {
            c.declare_method("p.S", "own");
            let l = Locals::new();
            // Déclaré après l'appel : toujours reconnu.
            c.record("p.S.m", &ty(), &l, Receiver::This, "own");
            c.record("p.S.m", &ty(), &l, Receiver::Implicit, "own");
            c.record("p.S.m", &ty(), &l, Receiver::Implicit, "imported");
        });
        let t: Vec<&str> = refs.iter().map(|r| r.1.as_str()).collect();
        assert_eq!(t, ["p.S.own", "imported"]);
    }

    #[test]
    fn type_englobant_externe_pour_champ_et_methode() {
        let types = vec!["p.O".to_string(), "p.O.I".to_string()];
        let refs = run(|c| {
            c.declare_field("p.O", "r", "R");
            c.declare_method("p.O", "outer");
            let l = Locals::new();
            c.record("p.O.I.m", &types, &l, Receiver::Name("r".into()), "a");
            c.record("p.O.I.m", &types, &l, Receiver::Implicit, "outer");
        });
        let t: Vec<&str> = refs.iter().map(|r| r.1.as_str()).collect();
        assert_eq!(t, ["R.a", "p.O.outer"]);
    }
}
