//! Every command has a method in every binding.
//!
//! A command is added in one place and has to appear in five: the Rust client, the C ABI, the
//! WebAssembly class, the Python class and the Dart class. Nothing in the compiler notices when one
//! of them is forgotten, because each binding is free to expose whatever it likes. This test
//! notices, and it names the binding that is behind.

use std::fs;
use std::path::{Path, PathBuf};

/// The repository root, or `None` when this runs from a packaged crate that has no bindings beside
/// it, which is what happens during `cargo publish`.
fn workspace_root() -> Option<PathBuf> {
    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    while dir.pop() {
        if dir.join("bindings").is_dir() && dir.join("crates").is_dir() {
            return Some(dir);
        }
    }
    None
}

/// Every variant of `Command`, in the order the enum declares them.
fn commands(root: &Path) -> Vec<String> {
    let source = fs::read_to_string(root.join("crates/obby-client/src/command.rs"))
        .expect("the command module is part of this crate");
    let start = source
        .find("pub enum Command {")
        .expect("the command enum is declared once");
    let body = &source[start..];
    let end = body.find("\n}\n").expect("the enum is closed");
    body[..end]
        .lines()
        .filter_map(|line| {
            let name = line.strip_prefix("    ")?;
            let name = name.strip_suffix(" {").or_else(|| name.strip_suffix(','))?;
            name.chars()
                .next()
                .is_some_and(char::is_uppercase)
                .then(|| name.to_owned())
        })
        .collect()
}

/// `SendMessage` as `send_message`.
fn snake(name: &str) -> String {
    let mut out = String::new();
    for (index, letter) in name.char_indices() {
        if letter.is_uppercase() && index > 0 {
            out.push('_');
        }
        out.extend(letter.to_lowercase());
    }
    out
}

/// `SendMessage` as `sendMessage`.
fn camel(name: &str) -> String {
    let mut letters = name.chars();
    let first: String = letters
        .by_ref()
        .take(1)
        .flat_map(char::to_lowercase)
        .collect();
    first + letters.as_str()
}

fn assert_every_command_appears(
    root: &Path,
    binding: &str,
    file: &str,
    spelling: fn(&str) -> String,
) {
    let source = fs::read_to_string(root.join(file))
        .unwrap_or_else(|_| panic!("{binding} has a source file at {file}"));
    let missing: Vec<String> = commands(root)
        .into_iter()
        .map(|command| spelling(&command))
        .filter(|method| !source.contains(&format!("{method}(")))
        .collect();
    assert!(
        missing.is_empty(),
        "{binding} has no method for: {}. Every command needs one, or a host has to fall back to \
         the JSON escape hatch for that one command alone.",
        missing.join(", ")
    );
}

#[test]
fn every_command_has_a_method_in_every_binding() {
    let Some(root) = workspace_root() else {
        return;
    };

    assert_every_command_appears(
        &root,
        "the Rust client",
        "crates/obby-client/src/client.rs",
        |command| format!("pub fn {}", snake(command)),
    );
    assert_every_command_appears(
        &root,
        "the C ABI",
        "bindings/obby-ffi/src/lib.rs",
        |command| format!("obby_client_{}", snake(command)),
    );
    assert_every_command_appears(
        &root,
        "the WebAssembly binding",
        "bindings/obby-wasm/src/lib.rs",
        |command| format!("pub fn {}", snake(command)),
    );
    assert_every_command_appears(
        &root,
        "the Python binding",
        "bindings/obby-python/src/lib.rs",
        |command| format!("fn {}", snake(command)),
    );
    assert_every_command_appears(
        &root,
        "the Dart binding",
        "bindings/obby-dart/lib/obby_client.dart",
        camel,
    );
}
