//! Fichiers YAML : clés de configuration et candidats de nom JVM, lus par événements sans
//! développer les alias (REQ-033, DEC-036).

use std::path::Path;

use yaml_rust2::parser::{Event, Parser};
use yaml_rust2::scanner::TScalarStyle;

use super::data::{self, Format};
use super::keys::KeySet;
use super::FileStructure;

/// Destination des candidats d'une valeur.
#[derive(Clone)]
enum Sink {
    Doc,
    Key(String),
    Nowhere,
}

enum Pending {
    /// Une clé est attendue.
    Key,
    /// Une valeur est attendue ; `None` = clé ignorée (fusion, clé complexe).
    Value(Option<String>),
}

enum Ctx {
    /// Mapping dont les clés sont des nœuds ; `leaf` = chemin à enregistrer s'il reste vide.
    Map {
        prefix: String,
        leaf: Option<String>,
        had_entries: bool,
        pending: Pending,
    },
    /// Mapping parcouru pour ses seuls candidats (valeurs ; les clés sont ignorées).
    Flat { sink: Sink, key_turn: bool },
    /// Séquence parcourue pour ses seuls candidats.
    Seq { sink: Sink },
}

struct Builder {
    keys: KeySet,
    stack: Vec<Ctx>,
}

impl Builder {
    fn sink_add(&mut self, sink: &Sink, value: &str) {
        match sink {
            Sink::Doc => self.keys.add_doc_value(value),
            Sink::Key(p) => self.keys.add_key_value(p, value),
            Sink::Nowhere => {}
        }
    }

    /// Un conteneur ou une valeur vient de se terminer dans le contexte courant.
    fn value_done(&mut self) {
        match self.stack.last_mut() {
            Some(Ctx::Map {
                pending,
                had_entries,
                ..
            }) => {
                *pending = match pending {
                    Pending::Key => Pending::Value(None),
                    Pending::Value(_) => {
                        *had_entries = true;
                        Pending::Key
                    }
                };
            }
            Some(Ctx::Flat { key_turn, .. }) => *key_turn = !*key_turn,
            _ => {}
        }
    }

    /// Décrit où va l'événement de valeur courant : `Err(())` si c'est une clé de mapping.
    fn slot(&mut self) -> Slot {
        match self.stack.last_mut() {
            None => Slot::Root,
            Some(Ctx::Map {
                prefix, pending, ..
            }) => match pending {
                Pending::Key => Slot::MapKey(prefix.clone()),
                Pending::Value(Some(p)) => Slot::Value(Sink::Key(p.clone()), Some(p.clone())),
                Pending::Value(None) => Slot::Value(Sink::Nowhere, None),
            },
            Some(Ctx::Flat { sink, key_turn }) => {
                if *key_turn {
                    Slot::FlatKey
                } else {
                    Slot::Value(sink.clone(), None)
                }
            }
            Some(Ctx::Seq { sink }) => Slot::Value(sink.clone(), None),
        }
    }

    fn event(&mut self, ev: Event) {
        match ev {
            Event::DocumentStart => self.stack.clear(),
            Event::Scalar(text, style, _, _) => match self.slot() {
                Slot::Root => self.keys.add_doc_value(&text),
                Slot::MapKey(prefix) => {
                    let ignored = text.is_empty() || (text == "<<" && style == TScalarStyle::Plain);
                    let value = if ignored {
                        None
                    } else if prefix.is_empty() {
                        Some(text)
                    } else {
                        Some(format!("{prefix}.{text}"))
                    };
                    if let Some(Ctx::Map {
                        pending,
                        had_entries,
                        ..
                    }) = self.stack.last_mut()
                    {
                        *pending = Pending::Value(value);
                        *had_entries = true;
                    }
                }
                Slot::FlatKey => self.value_done(),
                Slot::Value(sink, leaf) => {
                    if let Some(p) = &leaf {
                        self.keys.add_key(p);
                    }
                    self.sink_add(&sink, &text);
                    self.value_done();
                }
            },
            Event::Alias(_) => match self.slot() {
                Slot::Root => {}
                Slot::MapKey(_) => {
                    if let Some(Ctx::Map { pending, .. }) = self.stack.last_mut() {
                        *pending = Pending::Value(None);
                    }
                }
                Slot::FlatKey => self.value_done(),
                Slot::Value(_, leaf) => {
                    if let Some(p) = &leaf {
                        self.keys.add_key(p);
                    }
                    self.value_done();
                }
            },
            Event::SequenceStart(..) => match self.slot() {
                Slot::Root => self.stack.push(Ctx::Seq { sink: Sink::Doc }),
                Slot::MapKey(_) | Slot::FlatKey => self.stack.push(Ctx::Seq {
                    sink: Sink::Nowhere,
                }),
                Slot::Value(sink, leaf) => {
                    if let Some(p) = &leaf {
                        self.keys.add_key(p);
                    }
                    self.stack.push(Ctx::Seq { sink });
                }
            },
            Event::MappingStart(..) => match self.slot() {
                Slot::Root => self.stack.push(Ctx::Map {
                    prefix: String::new(),
                    leaf: None,
                    had_entries: false,
                    pending: Pending::Key,
                }),
                Slot::MapKey(_) | Slot::FlatKey => self.stack.push(Ctx::Flat {
                    sink: Sink::Nowhere,
                    key_turn: true,
                }),
                Slot::Value(_, Some(p)) => self.stack.push(Ctx::Map {
                    prefix: p.clone(),
                    leaf: Some(p),
                    had_entries: false,
                    pending: Pending::Key,
                }),
                Slot::Value(sink, None) => self.stack.push(Ctx::Flat {
                    sink,
                    key_turn: true,
                }),
            },
            Event::SequenceEnd | Event::MappingEnd => {
                if let Some(Ctx::Map {
                    leaf: Some(p),
                    had_entries: false,
                    ..
                }) = self.stack.pop()
                {
                    self.keys.add_key(&p);
                }
                self.value_done();
            }
            _ => {}
        }
    }
}

enum Slot {
    Root,
    /// Position de clé dans un mapping de clés (préfixe courant).
    MapKey(String),
    /// Position de clé dans un mapping ignoré.
    FlatKey,
    /// Position de valeur : destination des candidats, et clé feuille éventuelle.
    Value(Sink, Option<String>),
}

/// Extrait le document, les clés et les mentions d'un fichier YAML.
pub fn extract(path: &Path, text: &str) -> FileStructure {
    let mut b = Builder {
        keys: KeySet::new(),
        stack: Vec::new(),
    };
    let mut parser = Parser::new_from_str(text);
    let mut error = None;
    loop {
        match parser.next_token() {
            Ok((Event::StreamEnd, _)) => break,
            Ok((ev, _)) => b.event(ev),
            Err(e) => {
                error = Some(e.to_string());
                break;
            }
        }
    }
    let mut st = b.keys.finish(path, Format::Yaml);
    if let Some(e) = error {
        st.warnings.push(data::warning(path, Format::Yaml, &e));
    }
    st
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(p: &str, text: &str) -> FileStructure {
        extract(Path::new(p), text)
    }

    fn keys(f: &FileStructure) -> Vec<String> {
        f.declares
            .iter()
            .filter_map(|d| d.short.clone().filter(|_| d.id().starts_with("key:")))
            .collect()
    }

    fn refs(f: &FileStructure) -> Vec<(String, String)> {
        f.references
            .iter()
            .map(|r| (r.source.clone(), r.target.clone()))
            .collect()
    }

    const APP: &str = "spring:\n  datasource:\n    url: jdbc:postgresql://db/app\n    driver-class-name: org.postgresql.Driver\nnephila:\n  listener: com.acme.core.OrderService\n  handlers:\n    - com.acme.core.OrderRepository\n    - name: x\n      impl: com.acme.core.OrderService\n";

    /// AC-033-1
    #[test]
    fn req033_ac1_cles_et_mentions() {
        let f = run("src/main/resources/application.yml", APP);
        assert_eq!(
            keys(&f),
            [
                "nephila.handlers",
                "nephila.listener",
                "spring.datasource.driver-class-name",
                "spring.datasource.url"
            ]
        );
        let d = "key:src/main/resources/application.yml#";
        let r = refs(&f);
        assert!(r.contains(&(
            format!("{d}nephila.listener"),
            "com.acme.core.OrderService".into()
        )));
        assert!(r.contains(&(
            format!("{d}nephila.handlers"),
            "com.acme.core.OrderRepository".into()
        )));
        assert!(r.contains(&(
            format!("{d}nephila.handlers"),
            "com.acme.core.OrderService".into()
        )));
        assert!(f.warnings.is_empty());
        assert_eq!(
            f.declares[0].id(),
            "document:src/main/resources/application.yml"
        );
    }

    /// AC-033-2
    #[test]
    fn req033_ac2_flux_multi_documents_yaml_et_yml() {
        let t = "kind: A\nmetadata:\n  name: a\n---\nkind: B\nmetadata:\n  name: b\n";
        for p in ["deploy/k8s.yaml", "deploy/k8s.yml"] {
            let f = run(p, t);
            assert_eq!(keys(&f), ["kind", "metadata.name"], "{p}");
            assert!(f.warnings.is_empty());
        }
    }

    /// AC-033-3
    #[test]
    fn req033_ac3_ancres_fusion_cles_non_chaines_et_complexes() {
        let t =
            "base: &b {x: 1}\nderived: *b\nmerged:\n  <<: *b\n  y: 2\n8080: port\n? [a, b]\n: v\n";
        let f = run("a.yaml", t);
        assert_eq!(keys(&f), ["8080", "base.x", "derived", "merged.y"]);
        assert!(f.warnings.is_empty(), "{:?}", f.warnings);
        assert!(f.references.is_empty());
    }

    /// AC-033-4
    #[test]
    fn req033_ac4_flux_non_ferme() {
        let f = run(
            "a.yaml",
            "a: 1\nb:\n  c: com.acme.core.OrderService\ne: [1, 2",
        );
        let k = keys(&f);
        assert!(
            k.contains(&"a".to_string()) && k.contains(&"b.c".to_string()),
            "{k:?}"
        );
        assert!(refs(&f).contains(&("key:a.yaml#b.c".into(), "com.acme.core.OrderService".into())));
        assert_eq!(f.warnings.len(), 1);
        assert!(
            f.warnings[0].starts_with("a.yaml : YAML mal formé"),
            "{:?}",
            f.warnings
        );
        assert!(f.warnings[0].contains("line"), "{:?}", f.warnings);
    }

    /// AC-033-5
    #[test]
    fn req033_ac5_blocs_litteral_et_replie() {
        let t = "desc: |\n  voir com.acme.core.OrderService\n  pour le détail\nfold: >\n  com.acme.B\n  x\n";
        let f = run("a.yaml", t);
        let r = refs(&f);
        assert!(r.contains(&(
            "key:a.yaml#desc".into(),
            "com.acme.core.OrderService".into()
        )));
        assert!(r.contains(&("key:a.yaml#fold".into(), "com.acme.B".into())));
    }

    #[test]
    fn plafond_de_1500_cles() {
        let t: String = (0..1_500).rev().map(|i| format!("k{i:04}: v\n")).collect();
        let f = run("big.yaml", &t);
        assert_eq!(keys(&f).len(), 1_000);
        assert_eq!(keys(&f)[0], "k0000");
        assert!(!keys(&f).iter().any(|k| k == "k1000"));
        assert_eq!(keys(&f)[999], "k0999");
        assert_eq!(
            f.warnings,
            ["big.yaml : 500 clés non retenues (plafond de 1 000)"]
        );
    }

    #[test]
    fn fichier_vide_ou_commentaire_document_seul() {
        for t in ["", "# rien\n", "---\n", "{}"] {
            let f = run("e.yaml", t);
            assert_eq!(f.declares.len(), 1, "{t:?}");
            assert!(f.warnings.is_empty() && f.references.is_empty());
        }
    }

    #[test]
    fn racine_scalaire_ou_sequence_candidats_du_document() {
        let f = run(
            "a.yaml",
            "- com.acme.A\n- {k: com.acme.B}\n- [com.acme.C]\n",
        );
        assert_eq!(f.declares.len(), 1);
        let t: Vec<String> = refs(&f).into_iter().map(|r| r.1).collect();
        assert_eq!(t, ["com.acme.A", "com.acme.B", "com.acme.C"]);
        let f = run("b.yaml", "com.acme.D\n");
        assert_eq!(refs(&f), [("document:b.yaml".into(), "com.acme.D".into())]);
    }

    #[test]
    fn mappings_vides_sequences_et_cle_de_fusion_ignoree() {
        let t =
            "a: {}\nb: []\nc:\n  <<: {x.Y: com.acme.Z}\nd: !!str com.acme.T\ne: &anc com.acme.U\n";
        let f = run("a.yaml", t);
        assert_eq!(keys(&f), ["a", "b", "d", "e"]);
        let t: Vec<String> = refs(&f).into_iter().map(|r| r.1).collect();
        assert_eq!(t, ["com.acme.T", "com.acme.U"]);
    }
}
