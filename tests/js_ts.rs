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
