//! JavaScript et TypeScript de bout en bout (REQ-034) : imports, spécifieurs et héritage.

use assert_cmd::cargo::cargo_bin;
use serde_json::Value;
use std::fs;
use std::path::Path;
use std::process::Command;

fn write(repo: &Path, rel: &str, text: &str) {
    let p = repo.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

fn graph(repo: &Path) -> Value {
    let run = |args: &[&str]| {
        Command::new(cargo_bin("nephila"))
            .arg("-C")
            .arg(repo)
            .args(args)
            .env("NEPHILA_CONFIG", repo.join("absent.toml"))
            .env_remove("NEPHILA_MOCK_DIR")
            .env_remove("NEPHILA_MOCK_LOG")
            .output()
            .unwrap()
    };
    let b = run(&["build"]);
    assert!(b.status.success(), "{}", String::from_utf8_lossy(&b.stderr));
    let e = run(&["export", "--output", "-"]);
    assert!(e.status.success(), "{}", String::from_utf8_lossy(&e.stderr));
    serde_json::from_slice(&e.stdout).unwrap()
}

fn has_node(v: &Value, id: &str) -> bool {
    v["nodes"].as_array().unwrap().iter().any(|n| n["id"] == id)
}

fn has_edge(v: &Value, s: &str, k: &str, t: &str) -> bool {
    v["edges"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["source"] == s && e["kind"] == k && e["target"] == t)
}

fn edges_of(v: &Value, kind: &str) -> Vec<String> {
    v["edges"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == kind)
        .map(|e| e.to_string())
        .collect()
}

fn unresolved(v: &Value) -> String {
    v["unresolved"].to_string()
}

#[test]
fn req034_ac1_imports_du_depot_web() {
    let d = tempfile::tempdir().unwrap();
    let r = d.path();
    write(r, "web/src/model/order.ts", "export interface Order {}\n");
    write(
        r,
        "web/src/services/order.repository.ts",
        "export class OrderRepository { save(o: Order) {} }\n",
    );
    write(
        r,
        "web/src/services/order.service.ts",
        "import { Order } from '../model/order';\nimport { OrderRepository } from './order.repository';\n\
         export class OrderService { constructor(private repo: OrderRepository) {} place(o: Order) { return this.repo.save(o); } }\n",
    );
    let v = graph(r);
    let pkg = "package:web/src/services/order.service";
    for id in [
        pkg,
        "type:web/src/services/order.service.OrderService",
        "method:web/src/services/order.service.OrderService.<init>",
        "method:web/src/services/order.service.OrderService.place",
    ] {
        assert!(has_node(&v, id), "{id}");
    }
    let node = |id: &str| {
        v["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["id"] == id)
            .unwrap()
            .clone()
    };
    let short = node(pkg)["short"].as_str().unwrap().to_string();
    assert_eq!(short, "order.service");
    assert_eq!(
        node("type:web/src/services/order.service.OrderService")["subkind"],
        "class"
    );
    assert!(has_edge(
        &v,
        pkg,
        "contains",
        "type:web/src/services/order.service.OrderService"
    ));
    assert!(has_edge(
        &v,
        "type:web/src/services/order.service.OrderService",
        "contains",
        "method:web/src/services/order.service.OrderService.place"
    ));
    assert!(has_edge(
        &v,
        pkg,
        "imports",
        "type:web/src/model/order.Order"
    ));
    assert!(has_edge(
        &v,
        pkg,
        "imports",
        "type:web/src/services/order.repository.OrderRepository"
    ));
}

#[test]
fn req034_ac6_externes_et_alias() {
    let d = tempfile::tempdir().unwrap();
    let r = d.path();
    write(r, "src/lib/utils.ts", "export function cn() {}\n");
    write(
        r,
        "src/page.tsx",
        "import React, { useState } from 'react';\nimport { readFile } from 'node:fs';\nimport { cn } from '@/lib/utils';\n\
         export class Page extends React.Component { render() { useState(); readFile(); cn(); } }\n",
    );
    let v = graph(r);
    assert!(has_edge(
        &v,
        "package:src/page",
        "imports",
        "method:src/lib/utils.cn"
    ));
    assert!(
        edges_of(&v, "extends").is_empty(),
        "{:?}",
        edges_of(&v, "extends")
    );
    assert_eq!(edges_of(&v, "imports").len(), 1);
    let u = unresolved(&v);
    for n in ["React", "Component", "useState", "readFile", "react", "fs"] {
        assert!(!u.contains(n), "{n} dans {u}");
    }
}

#[test]
fn req034_ac7_specifieurs() {
    let d = tempfile::tempdir().unwrap();
    let r = d.path();
    write(r, "src/services/index.ts", "export * from './x';\n");
    write(r, "src/services/x.ts", "export class X { run() {} }\n");
    write(r, "src/b.ts", "export function b() {}\n");
    write(r, "src/styles.css", "a {}\n");
    write(r, "src/data.json", "{}\n");
    write(
        r,
        "src/a.ts",
        "import { X } from './services';\nimport { b } from './b.js';\nimport './styles.css';\nimport data from './data.json';\n\
         export function f() { b(); }\n",
    );
    let v = graph(r);
    assert!(has_edge(
        &v,
        "package:src/a",
        "imports",
        "type:src/services/x.X"
    ));
    assert!(has_edge(&v, "package:src/a", "imports", "method:src/b.b"));
    assert_eq!(
        edges_of(&v, "imports").len(),
        2,
        "{:?}",
        edges_of(&v, "imports")
    );
    assert!(!unresolved(&v).contains("data"));
}

#[test]
fn req034_ac7_nom_ambigu_reste_non_resolu() {
    let d = tempfile::tempdir().unwrap();
    let r = d.path();
    write(r, "src/services/index.ts", "export * from './x';\n");
    write(r, "src/services/x.ts", "export class X {}\n");
    write(r, "src/other/x.ts", "export class X {}\n");
    write(r, "src/a.ts", "import { X } from './services';\n");
    let v = graph(r);
    assert!(edges_of(&v, "imports").is_empty());
    assert!(unresolved(&v).contains("\"X\"") || unresolved(&v).contains("X"));
}

#[test]
fn req034_extends_espace_de_noms() {
    let d = tempfile::tempdir().unwrap();
    let r = d.path();
    write(r, "src/base.ts", "export class B {}\n");
    write(
        r,
        "src/c.ts",
        "import * as ns from './base';\nexport class C extends ns.B {}\n",
    );
    let v = graph(r);
    assert!(has_edge(&v, "type:src/c.C", "extends", "type:src/base.B"));
}

#[test]
fn req034_ac1_appel_via_champ_type() {
    let d = tempfile::tempdir().unwrap();
    let r = d.path();
    write(r, "web/src/model/order.ts", "export interface Order {}\n");
    write(
        r,
        "web/src/services/order.repository.ts",
        "export class OrderRepository { save(o: Order) {} }\n",
    );
    write(
        r,
        "web/src/services/order.service.ts",
        "import { Order } from '../model/order';\nimport { OrderRepository } from './order.repository';\n\
         export class OrderService { constructor(private repo: OrderRepository) {} place(o: Order) { return this.repo.save(o); } }\n",
    );
    let v = graph(r);
    assert!(has_edge(
        &v,
        "method:web/src/services/order.service.OrderService.place",
        "calls",
        "method:web/src/services/order.repository.OrderRepository.save"
    ));
    assert_eq!(unresolved(&v), "[]");
}

#[test]
fn req034_ac3_ac4_ac5_appels_et_jsx() {
    let d = tempfile::tempdir().unwrap();
    let r = d.path();
    write(
        r,
        "src/util/format.ts",
        "function helper() {}\nconst slug = (s: string) => s;\n\
         export function formatDate(d: Date) { helper(); slug('x'); console.log(d); fetch('/x'); [1].map(() => helper()); }\nhelper();\n",
    );
    write(
        r,
        "src/server.cjs",
        "const { formatDate: fd } = require('./util/format');\nconst util = require('./util/format');\n\
         function start() { fd(); util.slug(); }\nmodule.exports = { start };\n",
    );
    write(
        r,
        "src/Button.jsx",
        "export default function Button() { return <button/>; }\n",
    );
    write(
        r,
        "src/App.jsx",
        "import Button from './Button';\nexport function App() { return <div><Button label=\"x\"/></div>; }\n",
    );
    let v = graph(r);
    let f = "src/util/format";
    assert!(has_edge(
        &v,
        &format!("method:{f}.formatDate"),
        "calls",
        &format!("method:{f}.helper")
    ));
    assert!(has_edge(
        &v,
        &format!("method:{f}.formatDate"),
        "calls",
        &format!("method:{f}.slug")
    ));
    assert!(has_edge(
        &v,
        "method:src/server.start",
        "calls",
        &format!("method:{f}.formatDate")
    ));
    assert!(has_edge(
        &v,
        "method:src/server.start",
        "calls",
        &format!("method:{f}.slug")
    ));
    assert!(has_edge(
        &v,
        "method:src/App.App",
        "calls",
        "method:src/Button.Button"
    ));
    assert_eq!(
        edges_of(&v, "calls").len(),
        5,
        "{:?}",
        edges_of(&v, "calls")
    );
    assert_eq!(unresolved(&v), "[]");
}

#[test]
fn req034_ac7_instance_via_barrel() {
    let d = tempfile::tempdir().unwrap();
    let r = d.path();
    write(r, "src/services/index.ts", "export * from './x';\n");
    write(r, "src/services/x.ts", "export class X { run() {} }\n");
    write(
        r,
        "src/a.ts",
        "import { X } from './services';\nexport function f() { const x = new X(); x.run(); }\n",
    );
    let v = graph(r);
    assert!(has_edge(
        &v,
        "method:src/a.f",
        "calls",
        "method:src/services/x.X.run"
    ));
    // Second `X` nu (sans `run`) : la référence d'appel est ambiguë, donc non résolue.
    write(r, "src/other/x.ts", "export class X {}\n");
    let v = graph(r);
    assert!(edges_of(&v, "calls").is_empty());
    assert!(unresolved(&v).contains("X.run"), "{}", unresolved(&v));
}

#[test]
fn req034_ac1_meme_graphe_avec_mock_et_no_llm() {
    let d = tempfile::tempdir().unwrap();
    let r = d.path();
    write(r, "web/src/model/order.ts", "export interface Order {}\n");
    write(
        r,
        "web/src/services/order.repository.ts",
        "export class OrderRepository { save(o: Order) {} }\n",
    );
    write(
        r,
        "web/src/services/order.service.ts",
        "import { Order } from '../model/order';\nimport { OrderRepository } from './order.repository';\n\
         export class OrderService { constructor(private repo: OrderRepository) {} place(o: Order) { return this.repo.save(o); } }\n",
    );
    let sans = graph(r);
    assert_eq!(unresolved(&sans), "[]");
    let out = tempfile::tempdir().unwrap();
    let cfg = out.path().join("mock.toml");
    fs::write(&cfg, "provider = \"mock\"\nmodel = \"mock\"\n").unwrap();
    let mock_dir = out.path().join("mock");
    fs::create_dir_all(&mock_dir).unwrap();
    let log = out.path().join("mock.log");
    let run = |args: &[&str]| {
        Command::new(cargo_bin("nephila"))
            .arg("-C")
            .arg(r)
            .args(args)
            .env("NEPHILA_CONFIG", &cfg)
            .env("NEPHILA_MOCK_DIR", &mock_dir)
            .env("NEPHILA_MOCK_LOG", &log)
            .output()
            .unwrap()
    };
    let b = run(&["build", "--no-llm"]);
    assert!(b.status.success(), "{}", String::from_utf8_lossy(&b.stderr));
    let e = run(&["export", "--output", "-"]);
    assert!(e.status.success(), "{}", String::from_utf8_lossy(&e.stderr));
    let avec: Value = serde_json::from_slice(&e.stdout).unwrap();
    for k in ["nodes", "edges", "unresolved"] {
        assert_eq!(avec[k], sans[k], "{k}");
    }
    assert!(has_edge(
        &avec,
        "method:web/src/services/order.service.OrderService.place",
        "calls",
        "method:web/src/services/order.repository.OrderRepository.save"
    ));
    let logged = fs::read_to_string(&log).unwrap_or_default();
    assert!(logged.trim().is_empty(), "{logged}");
}

#[test]
fn req034_ac5_jsx_liste_et_item() {
    let d = tempfile::tempdir().unwrap();
    let r = d.path();
    write(
        r,
        "src/components/Item.tsx",
        "export const Item = () => <li/>;\n",
    );
    write(
        r,
        "src/components/List.tsx",
        "import { Item } from './Item';\ntype Props = { items: string[] };\n\
         export const List = ({ items }: Props) => <ul>{items.map(i => <Item key={i}/>)}</ul>;\n",
    );
    let v = graph(r);
    assert!(has_edge(
        &v,
        "method:src/components/List.List",
        "calls",
        "method:src/components/Item.Item"
    ));
    assert_eq!(edges_of(&v, "calls").len(), 1);
}

#[test]
fn req034_ac6_page_render_appelle_cn() {
    let d = tempfile::tempdir().unwrap();
    let r = d.path();
    write(r, "src/lib/utils.ts", "export function cn() {}\n");
    write(
        r,
        "src/page.tsx",
        "import React, { useState } from 'react';\nimport { readFile } from 'node:fs';\nimport { cn } from '@/lib/utils';\n\
         export class Page extends React.Component { render() { useState(); readFile(); cn(); } }\n",
    );
    let v = graph(r);
    assert!(has_edge(
        &v,
        "method:src/page.Page.render",
        "calls",
        "method:src/lib/utils.cn"
    ));
    assert_eq!(edges_of(&v, "calls").len(), 1);
}

#[test]
fn req034_ac7_alias_importe_appele() {
    let d = tempfile::tempdir().unwrap();
    let r = d.path();
    write(r, "src/m.ts", "export function a() {}\n");
    write(
        r,
        "src/u.ts",
        "import { a as c } from './m';\nexport function g() { c(); }\n",
    );
    let v = graph(r);
    assert!(has_edge(&v, "method:src/u.g", "calls", "method:src/m.a"));
}

// ---- package.json (REQ-035) ----

fn nephila_in(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(cargo_bin("nephila"))
        .current_dir(dir)
        .args(args)
        .env("NEPHILA_CONFIG", dir.join("absent.toml"))
        .env_remove("NEPHILA_MOCK_DIR")
        .env_remove("NEPHILA_MOCK_LOG")
        .output()
        .unwrap()
}

fn cmd_out(repo: &Path, args: &[&str]) -> std::process::Output {
    Command::new(cargo_bin("nephila"))
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("NEPHILA_CONFIG", repo.join("absent.toml"))
        .env_remove("NEPHILA_MOCK_DIR")
        .env_remove("NEPHILA_MOCK_LOG")
        .output()
        .unwrap()
}

fn stdout(o: &std::process::Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn exported(repo: &Path) -> Value {
    let e = cmd_out(repo, &["export", "--output", "-"]);
    assert!(e.status.success(), "{}", String::from_utf8_lossy(&e.stderr));
    serde_json::from_slice(&e.stdout).unwrap()
}

fn web_core_repo(r: &Path) {
    write(
        r,
        "web/package.json",
        r#"{"name": "@acme/web", "dependencies": {"react": "^18.3.0", "@acme/core": "workspace:*"}, "devDependencies": {"typescript": "5.6.0"}}"#,
    );
    write(r, "core/package.json", r#"{"name": "@acme/core"}"#);
    write(
        r,
        "web/src/services/order.service.ts",
        "export class OrderService {}\n",
    );
}

#[test]
fn req035_ac1_ac4_ac5_modules_dependances_et_requetes() {
    let d = tempfile::tempdir().unwrap();
    let r = d.path();
    web_core_repo(r);
    let v = graph(r);
    assert!(has_node(&v, "module:@acme/web") && has_node(&v, "module:@acme/core"));
    for t in [
        "module:@acme/core",
        "dependency:react",
        "dependency:typescript",
    ] {
        assert!(has_edge(&v, "module:@acme/web", "depends_on", t), "{t}");
    }
    let short = |id: &str| {
        v["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["id"] == id)
            .unwrap()["short"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert_eq!(short("module:@acme/web"), "web");
    assert_eq!(short("module:@acme/core"), "core");
    assert_eq!(unresolved(&v), "[]");
    // AC-035-4 : rattachement au module, puis disparition de la dépendance retirée.
    assert!(has_edge(
        &v,
        "module:@acme/web",
        "contains",
        "package:web/src/services/order.service"
    ));
    // AC-035-5
    let s = stdout(&cmd_out(r, &["summary"]));
    let bloc: Vec<&str> = s
        .lines()
        .skip_while(|l| !l.starts_with("Modules ("))
        .skip(1)
        .take_while(|l| l.starts_with("  "))
        .collect();
    assert!(
        bloc.iter()
            .any(|l| l.trim_start().starts_with("@acme/web ->")
                && l.contains("module:@acme/core")
                && l.contains("dependency:react")
                && l.contains("dependency:typescript")),
        "{s}"
    );
    assert!(bloc.iter().any(|l| l.trim() == "@acme/core"), "{s}");
    let n = stdout(&cmd_out(r, &["query", "neighbors", "@acme/web"]));
    let uses: Vec<&str> = n
        .lines()
        .skip_while(|l| *l != "utilise :")
        .skip(1)
        .take_while(|l| l.starts_with("  "))
        .collect();
    for t in ["@acme/core", "react", "typescript"] {
        assert!(
            uses.iter()
                .any(|l| l.trim_start().starts_with(&format!("depends_on {t}"))),
            "{t} : {n}"
        );
    }
    write(
        r,
        "web/package.json",
        r#"{"name": "@acme/web", "dependencies": {"@acme/core": "workspace:*"}, "devDependencies": {"typescript": "5.6.0"}}"#,
    );
    let u = cmd_out(r, &["update", "--no-llm"]);
    assert!(u.status.success(), "{}", String::from_utf8_lossy(&u.stderr));
    let v = exported(r);
    assert!(!has_node(&v, "dependency:react"));
    assert!(has_node(&v, "dependency:typescript"));
}

#[test]
fn req035_ac2_sections_dedoublonnees_et_ignorees() {
    let d = tempfile::tempdir().unwrap();
    let r = d.path();
    write(
        r,
        "package.json",
        r#"{"name":"app","dependencies":{"a":"1"},"devDependencies":{"a":"2"},
        "peerDependencies":{"p":"*"},"optionalDependencies":{"o":"1"},
        "scripts":{"build":"x"},"workspaces":["w"],"engines":{"node":">=20"}}"#,
    );
    let v = graph(r);
    let mut deps: Vec<String> = v["edges"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "depends_on")
        .map(|e| e["target"].as_str().unwrap().to_string())
        .collect();
    deps.sort();
    assert_eq!(deps, ["dependency:a", "dependency:o", "dependency:p"]);
}

#[test]
fn req035_ac2_module_sans_name_prend_le_dossier_du_depot() {
    let d = tempfile::tempdir().unwrap();
    let repo = d.path().join("monrepo");
    write(&repo, "package.json", r#"{"private": true}"#);
    fs::create_dir_all(repo.join("sub")).unwrap();
    let b = nephila_in(&repo, &["build", "--no-llm"]);
    assert!(b.status.success(), "{}", String::from_utf8_lossy(&b.stderr));
    assert!(has_node(&exported(&repo), "module:monrepo"));
    fs::remove_dir_all(repo.join(".nephila")).unwrap();
    let b = nephila_in(&repo.join("sub"), &["-C", "..", "build", "--no-llm"]);
    assert!(b.status.success(), "{}", String::from_utf8_lossy(&b.stderr));
    assert!(has_node(&exported(&repo), "module:monrepo"));
}

#[test]
fn req035_ac3_package_json_tronque_puis_corrige() {
    let d = tempfile::tempdir().unwrap();
    let r = d.path();
    write(r, "package.json", "{\"name\": \"x\",");
    write(
        r,
        "src/a.ts",
        "export const a = 1;\nexport function f() {}\n",
    );
    let b = cmd_out(r, &["build", "--no-llm"]);
    assert!(b.status.success());
    let err = String::from_utf8_lossy(&b.stderr).into_owned();
    assert!(
        err.contains("avertissement:") && err.contains("package.json"),
        "{err}"
    );
    let v = exported(r);
    assert!(v["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .all(|n| !n["id"].as_str().unwrap().starts_with("module:")));
    assert!(!stdout(&cmd_out(r, &["status"])).contains("package.json"));
    write(r, "package.json", r#"{"name": "x"}"#);
    let u = cmd_out(r, &["update", "--no-llm"]);
    assert!(u.status.success());
    let v = exported(r);
    assert!(has_edge(&v, "module:x", "contains", "package:src/a"));
}

#[test]
fn req035_ac6_mise_a_niveau_d_un_package_json_document() {
    use nephila::model::{FileRecord, FileState, Node, NodeKind, SubKind};
    use nephila::store::Store;
    let d = tempfile::tempdir().unwrap();
    let r = d.path();
    write(r, "README.md", "# Titre\n");
    let b = cmd_out(r, &["build", "--no-llm"]);
    assert!(b.status.success());
    let text = r#"{"name": "@acme/web", "dependencies": {"react": "^18"}}"#;
    write(r, "web/package.json", text);
    let hash = blake3::hash(text.as_bytes()).to_hex().to_string();
    {
        let mut store = Store::open_existing(r).unwrap();
        store
            .insert_files(&[FileRecord {
                path: "web/package.json".into(),
                hash,
                size: text.len() as u64,
                mtime: 1,
                recorded_at: 0,
                state: FileState::Indexed,
            }])
            .unwrap();
        let doc = Node::new(
            NodeKind::Document,
            "web/package.json",
            "package.json",
            Some("web/package.json"),
            Some(SubKind::Json),
            None,
        );
        let key = Node::new(
            NodeKind::Key,
            "web/package.json#name",
            "name",
            Some("web/package.json"),
            None,
            None,
        );
        store
            .replace_file("web/package.json", &[doc, key], &[])
            .unwrap();
    }
    let s = stdout(&cmd_out(r, &["status"]));
    assert_eq!(
        s.lines()
            .filter(|l| !l.contains("à enrichir"))
            .collect::<Vec<_>>(),
        ["graphe pas à jour", "modifié\tweb/package.json"],
        "{s}"
    );
    let u = cmd_out(r, &["update", "--no-llm"]);
    assert!(u.status.success(), "{}", String::from_utf8_lossy(&u.stderr));
    let uo = stdout(&u);
    // Seul `package.json` est réanalysé (README.md, inchangé, ne l'est pas).
    assert!(uo.contains(": 1 fichiers traités"), "{uo}");
    let v = exported(r);
    assert!(!has_node(&v, "document:web/package.json"));
    assert!(!has_node(&v, "key:web/package.json#name"));
    assert!(has_edge(
        &v,
        "module:@acme/web",
        "depends_on",
        "dependency:react"
    ));
    assert_eq!(
        stdout(&cmd_out(r, &["status"])).lines().next(),
        Some("graphe à jour")
    );
}

#[test]
fn req032_ac6_package_json_n_est_pas_un_document() {
    let d = tempfile::tempdir().unwrap();
    let r = d.path();
    write(r, "web/package.json", r#"{"name": "w", "version": "1"}"#);
    write(
        r,
        "web/tsconfig.json",
        r#"{"compilerOptions": {"strict": true}}"#,
    );
    let v = graph(r);
    assert!(!has_node(&v, "document:web/package.json"));
    assert!(v["nodes"].as_array().unwrap().iter().all(|n| !n["id"]
        .as_str()
        .unwrap()
        .starts_with("key:web/package.json#")));
    assert!(has_node(&v, "document:web/tsconfig.json"));
    assert!(v["nodes"].as_array().unwrap().iter().any(|n| n["id"]
        .as_str()
        .unwrap()
        .starts_with("key:web/tsconfig.json#")));
}

#[test]
fn req005_ac5_modules_jvm_et_npm_ne_se_melangent_pas() {
    let d = tempfile::tempdir().unwrap();
    let r = d.path();
    write(
        r,
        "pom.xml",
        "<project><groupId>com.acme</groupId><artifactId>app</artifactId></project>",
    );
    write(r, "package.json", r#"{"name": "app-front"}"#);
    write(
        r,
        "src/main/java/com/acme/A.java",
        "package com.acme;\npublic class A {}\n",
    );
    write(r, "web/src/main.ts", "export const x = 1;\n");
    let v = graph(r);
    assert!(has_edge(
        &v,
        "module:com.acme:app",
        "contains",
        "package:com.acme"
    ));
    assert!(has_edge(
        &v,
        "module:app-front",
        "contains",
        "package:web/src/main"
    ));
    let n = v["edges"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "contains" && e["source"].as_str().unwrap().starts_with("module:"))
        .count();
    assert_eq!(n, 2);
}
