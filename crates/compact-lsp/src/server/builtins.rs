// This file is part of compact-lsp.
// Copyright (C) 2025 Midnight Foundation
// SPDX-License-Identifier: Apache-2.0

//! Built-in type method registry for dot-completion.
//!
//! Method definitions live in `builtins.toml` (embedded at compile time).
//! This module parses the TOML once on first access and exposes the same
//! public API as before.

use serde::Deserialize;
use std::sync::OnceLock;

/// A built-in method available on a Compact type.
pub struct BuiltinMethod {
    /// Method name (e.g., "increment").
    pub name: &'static str,
    /// Display signature (e.g., "increment(amount: Uint<64>)").
    pub signature: &'static str,
    /// Snippet for insertion (e.g., "increment(${1:amount})").
    pub snippet: &'static str,
    /// Documentation string.
    pub documentation: &'static str,
}

// ── TOML deserialization structures ─────────────────────────────────

#[derive(Deserialize)]
struct Registry {
    types: Vec<TypeDef>,
}

#[derive(Deserialize)]
struct TypeDef {
    name: String,
    methods: Vec<MethodDef>,
}

#[derive(Deserialize)]
struct MethodDef {
    name: String,
    signature: String,
    snippet: String,
    doc: String,
}

// ── Parsed + converted registry ─────────────────────────────────────

struct ParsedRegistry {
    types: Vec<ParsedType>,
}

struct ParsedType {
    name: String,
    methods: Vec<BuiltinMethod>,
}

static REGISTRY: OnceLock<ParsedRegistry> = OnceLock::new();

fn registry() -> &'static ParsedRegistry {
    REGISTRY.get_or_init(|| {
        // The TOML source is embedded at compile time — no runtime I/O.
        let raw: Registry =
            toml::from_str(include_str!("builtins.toml")).expect("builtins.toml is invalid TOML");

        // Leak strings so BuiltinMethod can hold &'static str references.
        // This happens exactly once for the lifetime of the process.
        let types = raw
            .types
            .into_iter()
            .map(|t| ParsedType {
                name: t.name,
                methods: t
                    .methods
                    .into_iter()
                    .map(|m| BuiltinMethod {
                        name: Box::leak(m.name.into_boxed_str()),
                        signature: Box::leak(m.signature.into_boxed_str()),
                        snippet: Box::leak(m.snippet.into_boxed_str()),
                        documentation: Box::leak(m.doc.into_boxed_str()),
                    })
                    .collect(),
            })
            .collect();

        ParsedRegistry { types }
    })
}

/// Return the built-in methods for a given base type name.
pub fn methods_for_type(type_name: &str) -> &'static [BuiltinMethod] {
    registry()
        .types
        .iter()
        .find(|t| t.name == type_name)
        .map(|t| t.methods.as_slice())
        .unwrap_or(&[])
}

/// Find a specific built-in method by type and method name.
pub fn find_method_by_name(type_name: &str, method_name: &str) -> Option<&'static BuiltinMethod> {
    methods_for_type(type_name)
        .iter()
        .find(|m| m.name == method_name)
}

/// Extract the base type name from a potentially parameterized type string.
///
/// For example, `"Map<Address, Uint<64>>"` returns `"Map"`,
/// and `"Counter"` returns `"Counter"`.
pub fn extract_base_type(type_str: &str) -> &str {
    match type_str.find('<') {
        Some(idx) => &type_str[..idx],
        None => type_str,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_base_type_simple() {
        assert_eq!(extract_base_type("Counter"), "Counter");
    }

    #[test]
    fn test_extract_base_type_parameterized() {
        assert_eq!(extract_base_type("Map<Address, Uint<64>>"), "Map");
    }

    #[test]
    fn test_extract_base_type_single_param() {
        assert_eq!(extract_base_type("Set<Address>"), "Set");
    }

    #[test]
    fn test_methods_for_counter() {
        let methods = methods_for_type("Counter");
        assert_eq!(methods.len(), 5);
        let names: Vec<&str> = methods.iter().map(|m| m.name).collect();
        assert!(names.contains(&"increment"));
        assert!(names.contains(&"decrement"));
        assert!(names.contains(&"read"));
        assert!(names.contains(&"lessThan"));
        assert!(names.contains(&"resetToDefault"));
    }

    #[test]
    fn test_methods_for_cell() {
        let methods = methods_for_type("Cell");
        assert_eq!(methods.len(), 3);
        let names: Vec<&str> = methods.iter().map(|m| m.name).collect();
        assert!(names.contains(&"read"));
        assert!(names.contains(&"write"));
        assert!(names.contains(&"resetToDefault"));
        // Verify old incorrect names are gone
        assert!(!names.contains(&"value"));
        assert!(!names.contains(&"set"));
    }

    #[test]
    fn test_methods_for_map() {
        let methods = methods_for_type("Map");
        assert_eq!(methods.len(), 8);
        let names: Vec<&str> = methods.iter().map(|m| m.name).collect();
        assert!(names.contains(&"insert"));
        assert!(names.contains(&"insertDefault"));
        assert!(names.contains(&"remove"));
        assert!(names.contains(&"lookup"));
        assert!(names.contains(&"member"));
        assert!(names.contains(&"isEmpty"));
        assert!(names.contains(&"size"));
        assert!(names.contains(&"resetToDefault"));
    }

    #[test]
    fn test_methods_for_set() {
        let methods = methods_for_type("Set");
        assert_eq!(methods.len(), 6);
        let names: Vec<&str> = methods.iter().map(|m| m.name).collect();
        assert!(names.contains(&"insert"));
        assert!(names.contains(&"remove"));
        assert!(names.contains(&"member"));
        assert!(names.contains(&"isEmpty"));
        assert!(names.contains(&"size"));
        assert!(names.contains(&"resetToDefault"));
    }

    #[test]
    fn test_methods_for_list() {
        let methods = methods_for_type("List");
        assert_eq!(methods.len(), 6);
        let names: Vec<&str> = methods.iter().map(|m| m.name).collect();
        assert!(names.contains(&"pushFront"));
        assert!(names.contains(&"popFront"));
        assert!(names.contains(&"head"));
        assert!(names.contains(&"length"));
        assert!(names.contains(&"isEmpty"));
        assert!(names.contains(&"resetToDefault"));
    }

    #[test]
    fn test_methods_for_merkle_tree() {
        let methods = methods_for_type("MerkleTree");
        assert_eq!(methods.len(), 8);
        let names: Vec<&str> = methods.iter().map(|m| m.name).collect();
        assert!(names.contains(&"insert"));
        assert!(names.contains(&"insertIndex"));
        assert!(names.contains(&"insertHash"));
        assert!(names.contains(&"insertHashIndex"));
        assert!(names.contains(&"insertIndexDefault"));
        assert!(names.contains(&"checkRoot"));
        assert!(names.contains(&"isFull"));
        assert!(names.contains(&"resetToDefault"));
    }

    #[test]
    fn test_methods_for_historic_merkle_tree() {
        let methods = methods_for_type("HistoricMerkleTree");
        assert_eq!(methods.len(), 9);
        let names: Vec<&str> = methods.iter().map(|m| m.name).collect();
        assert!(names.contains(&"insert"));
        assert!(names.contains(&"insertIndex"));
        assert!(names.contains(&"insertHash"));
        assert!(names.contains(&"insertHashIndex"));
        assert!(names.contains(&"insertIndexDefault"));
        assert!(names.contains(&"checkRoot"));
        assert!(names.contains(&"isFull"));
        assert!(names.contains(&"resetToDefault"));
        assert!(names.contains(&"resetHistory"));
    }

    #[test]
    fn test_methods_for_kernel() {
        let methods = methods_for_type("Kernel");
        assert_eq!(methods.len(), 9);
        let names: Vec<&str> = methods.iter().map(|m| m.name).collect();
        assert!(names.contains(&"self"));
        assert!(names.contains(&"mint"));
        assert!(names.contains(&"blockTimeGreaterThan"));
        assert!(names.contains(&"blockTimeLessThan"));
        assert!(names.contains(&"checkpoint"));
        assert!(names.contains(&"claimContractCall"));
        assert!(names.contains(&"claimZswapCoinReceive"));
        assert!(names.contains(&"claimZswapCoinSpend"));
        assert!(names.contains(&"claimZswapNullifier"));
    }

    #[test]
    fn test_methods_for_unknown() {
        let methods = methods_for_type("Boolean");
        assert!(methods.is_empty());
    }

    #[test]
    fn test_find_method_by_name_found() {
        let method = find_method_by_name("Counter", "increment");
        assert!(method.is_some());
        assert_eq!(method.unwrap().name, "increment");
    }

    #[test]
    fn test_find_method_by_name_not_found() {
        assert!(find_method_by_name("Counter", "unknown_method").is_none());
    }

    #[test]
    fn test_find_method_by_name_unknown_type() {
        assert!(find_method_by_name("Boolean", "increment").is_none());
    }

    #[test]
    fn test_all_methods_have_required_fields() {
        for ptype in &registry().types {
            assert!(!ptype.name.is_empty(), "Type name must not be empty");
            assert!(
                !ptype.methods.is_empty(),
                "Type {} must have at least one method",
                ptype.name
            );
            for method in &ptype.methods {
                assert!(
                    !method.name.is_empty(),
                    "{}.{}: name must not be empty",
                    ptype.name,
                    method.name
                );
                assert!(
                    !method.signature.is_empty(),
                    "{}.{}: signature must not be empty",
                    ptype.name,
                    method.name
                );
                assert!(
                    !method.snippet.is_empty(),
                    "{}.{}: snippet must not be empty",
                    ptype.name,
                    method.name
                );
                assert!(
                    !method.documentation.is_empty(),
                    "{}.{}: documentation must not be empty",
                    ptype.name,
                    method.name
                );
            }
        }
    }
}
