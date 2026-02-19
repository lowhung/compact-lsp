// This file is part of compact-lsp.
// Copyright (C) 2025 Midnight Foundation
// SPDX-License-Identifier: Apache-2.0

//! Built-in type method registry for dot-completion.

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

/// Return the built-in methods for a given base type name.
pub fn methods_for_type(type_name: &str) -> &'static [BuiltinMethod] {
    match type_name {
        "Counter" => &[
            BuiltinMethod {
                name: "increment",
                signature: "increment(amount: Uint<16>)",
                snippet: "increment(${1:amount})",
                documentation: "Increment the counter by the given amount.",
            },
            BuiltinMethod {
                name: "decrement",
                signature: "decrement(amount: Uint<16>)",
                snippet: "decrement(${1:amount})",
                documentation: "Decrement the counter by the given amount.",
            },
            BuiltinMethod {
                name: "read",
                signature: "read(): Uint<64>",
                snippet: "read()",
                documentation: "Retrieve the current counter value.",
            },
            BuiltinMethod {
                name: "lessThan",
                signature: "lessThan(threshold: Uint<64>): Boolean",
                snippet: "lessThan(${1:threshold})",
                documentation: "Check if the counter is less than a threshold.",
            },
            BuiltinMethod {
                name: "resetToDefault",
                signature: "resetToDefault()",
                snippet: "resetToDefault()",
                documentation: "Reset the counter to zero.",
            },
        ],
        "Cell" => &[
            BuiltinMethod {
                name: "read",
                signature: "read(): T",
                snippet: "read()",
                documentation: "Retrieve the current value of the cell.",
            },
            BuiltinMethod {
                name: "write",
                signature: "write(value: T)",
                snippet: "write(${1:value})",
                documentation: "Overwrite the cell contents.",
            },
            BuiltinMethod {
                name: "resetToDefault",
                signature: "resetToDefault()",
                snippet: "resetToDefault()",
                documentation: "Reset to the type's default value.",
            },
        ],
        "Map" => &[
            BuiltinMethod {
                name: "insert",
                signature: "insert(key: K, value: V)",
                snippet: "insert(${1:key}, ${2:value})",
                documentation: "Insert or update a key-value pair.",
            },
            BuiltinMethod {
                name: "insertDefault",
                signature: "insertDefault(key: K)",
                snippet: "insertDefault(${1:key})",
                documentation: "Insert a key with the value type's default value.",
            },
            BuiltinMethod {
                name: "remove",
                signature: "remove(key: K)",
                snippet: "remove(${1:key})",
                documentation: "Remove a key-value pair.",
            },
            BuiltinMethod {
                name: "lookup",
                signature: "lookup(key: K): V",
                snippet: "lookup(${1:key})",
                documentation: "Look up the value for a given key.",
            },
            BuiltinMethod {
                name: "member",
                signature: "member(key: K): Boolean",
                snippet: "member(${1:key})",
                documentation: "Check if a key exists in the map.",
            },
            BuiltinMethod {
                name: "isEmpty",
                signature: "isEmpty(): Boolean",
                snippet: "isEmpty()",
                documentation: "Check if the map is empty.",
            },
            BuiltinMethod {
                name: "size",
                signature: "size(): Uint<64>",
                snippet: "size()",
                documentation: "Get the number of entries in the map.",
            },
            BuiltinMethod {
                name: "resetToDefault",
                signature: "resetToDefault()",
                snippet: "resetToDefault()",
                documentation: "Clear the map to empty.",
            },
        ],
        "Set" => &[
            BuiltinMethod {
                name: "insert",
                signature: "insert(element: T)",
                snippet: "insert(${1:element})",
                documentation: "Add an element to the set.",
            },
            BuiltinMethod {
                name: "remove",
                signature: "remove(element: T)",
                snippet: "remove(${1:element})",
                documentation: "Remove an element from the set.",
            },
            BuiltinMethod {
                name: "member",
                signature: "member(element: T): Boolean",
                snippet: "member(${1:element})",
                documentation: "Check if an element is in the set.",
            },
            BuiltinMethod {
                name: "isEmpty",
                signature: "isEmpty(): Boolean",
                snippet: "isEmpty()",
                documentation: "Check if the set is empty.",
            },
            BuiltinMethod {
                name: "size",
                signature: "size(): Uint<64>",
                snippet: "size()",
                documentation: "Get the number of elements in the set.",
            },
            BuiltinMethod {
                name: "resetToDefault",
                signature: "resetToDefault()",
                snippet: "resetToDefault()",
                documentation: "Clear the set to empty.",
            },
        ],
        "List" => &[
            BuiltinMethod {
                name: "pushFront",
                signature: "pushFront(value: T)",
                snippet: "pushFront(${1:value})",
                documentation: "Prepend an element to the front of the list.",
            },
            BuiltinMethod {
                name: "popFront",
                signature: "popFront()",
                snippet: "popFront()",
                documentation: "Remove the first element from the list.",
            },
            BuiltinMethod {
                name: "head",
                signature: "head(): Maybe<T>",
                snippet: "head()",
                documentation: "Retrieve the first element, or nothing if the list is empty.",
            },
            BuiltinMethod {
                name: "length",
                signature: "length(): Uint<64>",
                snippet: "length()",
                documentation: "Get the number of elements in the list.",
            },
            BuiltinMethod {
                name: "isEmpty",
                signature: "isEmpty(): Boolean",
                snippet: "isEmpty()",
                documentation: "Check if the list is empty.",
            },
            BuiltinMethod {
                name: "resetToDefault",
                signature: "resetToDefault()",
                snippet: "resetToDefault()",
                documentation: "Clear the list to empty.",
            },
        ],
        "MerkleTree" => &[
            BuiltinMethod {
                name: "insert",
                signature: "insert(item: T)",
                snippet: "insert(${1:item})",
                documentation: "Insert a leaf at the first free index.",
            },
            BuiltinMethod {
                name: "insertIndex",
                signature: "insertIndex(item: T, index: Uint<64>)",
                snippet: "insertIndex(${1:item}, ${2:index})",
                documentation: "Insert a leaf at a specific index.",
            },
            BuiltinMethod {
                name: "insertHash",
                signature: "insertHash(hash: Bytes<32>)",
                snippet: "insertHash(${1:hash})",
                documentation: "Insert a hash digest at the first free index.",
            },
            BuiltinMethod {
                name: "insertHashIndex",
                signature: "insertHashIndex(hash: Bytes<32>, index: Uint<64>)",
                snippet: "insertHashIndex(${1:hash}, ${2:index})",
                documentation: "Insert a hash digest at a specific index.",
            },
            BuiltinMethod {
                name: "insertIndexDefault",
                signature: "insertIndexDefault(index: Uint<64>)",
                snippet: "insertIndexDefault(${1:index})",
                documentation: "Insert default value at index (emulates removal).",
            },
            BuiltinMethod {
                name: "checkRoot",
                signature: "checkRoot(root: MerkleTreeDigest): Boolean",
                snippet: "checkRoot(${1:root})",
                documentation: "Validate a root against the current Merkle root.",
            },
            BuiltinMethod {
                name: "isFull",
                signature: "isFull(): Boolean",
                snippet: "isFull()",
                documentation: "Check if the tree is at capacity.",
            },
            BuiltinMethod {
                name: "resetToDefault",
                signature: "resetToDefault()",
                snippet: "resetToDefault()",
                documentation: "Clear the tree to empty.",
            },
        ],
        "HistoricMerkleTree" => &[
            BuiltinMethod {
                name: "insert",
                signature: "insert(item: T)",
                snippet: "insert(${1:item})",
                documentation: "Insert a leaf at the first free index.",
            },
            BuiltinMethod {
                name: "insertIndex",
                signature: "insertIndex(item: T, index: Uint<64>)",
                snippet: "insertIndex(${1:item}, ${2:index})",
                documentation: "Insert a leaf at a specific index.",
            },
            BuiltinMethod {
                name: "insertHash",
                signature: "insertHash(hash: Bytes<32>)",
                snippet: "insertHash(${1:hash})",
                documentation: "Insert a hash digest at the first free index.",
            },
            BuiltinMethod {
                name: "insertHashIndex",
                signature: "insertHashIndex(hash: Bytes<32>, index: Uint<64>)",
                snippet: "insertHashIndex(${1:hash}, ${2:index})",
                documentation: "Insert a hash digest at a specific index.",
            },
            BuiltinMethod {
                name: "insertIndexDefault",
                signature: "insertIndexDefault(index: Uint<64>)",
                snippet: "insertIndexDefault(${1:index})",
                documentation: "Insert default value at index (emulates removal).",
            },
            BuiltinMethod {
                name: "checkRoot",
                signature: "checkRoot(root: MerkleTreeDigest): Boolean",
                snippet: "checkRoot(${1:root})",
                documentation: "Validate a root against any historical Merkle root.",
            },
            BuiltinMethod {
                name: "isFull",
                signature: "isFull(): Boolean",
                snippet: "isFull()",
                documentation: "Check if the tree is at capacity.",
            },
            BuiltinMethod {
                name: "resetToDefault",
                signature: "resetToDefault()",
                snippet: "resetToDefault()",
                documentation: "Clear the tree to empty.",
            },
            BuiltinMethod {
                name: "resetHistory",
                signature: "resetHistory()",
                snippet: "resetHistory()",
                documentation: "Clear history, preserving only the current root.",
            },
        ],
        "Kernel" => &[
            BuiltinMethod {
                name: "self",
                signature: "self(): ContractAddress",
                snippet: "self()",
                documentation: "Return the current contract's address.",
            },
            BuiltinMethod {
                name: "mint",
                signature: "mint(domain_sep: Bytes<32>, amount: Uint<64>)",
                snippet: "mint(${1:domain_sep}, ${2:amount})",
                documentation: "Create shielded coins with a contract-derived token type.",
            },
            BuiltinMethod {
                name: "blockTimeGreaterThan",
                signature: "blockTimeGreaterThan(time: Uint<64>): Boolean",
                snippet: "blockTimeGreaterThan(${1:time})",
                documentation: "Check if the current block time exceeds a given timestamp.",
            },
            BuiltinMethod {
                name: "blockTimeLessThan",
                signature: "blockTimeLessThan(time: Uint<64>): Boolean",
                snippet: "blockTimeLessThan(${1:time})",
                documentation: "Check if the current block time is before a given timestamp.",
            },
            BuiltinMethod {
                name: "checkpoint",
                signature: "checkpoint()",
                snippet: "checkpoint()",
                documentation: "Mark execution as an atomic unit for partial rollback.",
            },
            BuiltinMethod {
                name: "claimContractCall",
                signature: "claimContractCall(addr: Bytes<32>, entry_point: Bytes<32>, comm: Field)",
                snippet: "claimContractCall(${1:addr}, ${2:entry_point}, ${3:comm})",
                documentation: "Require a matching contract call in the transaction.",
            },
            BuiltinMethod {
                name: "claimZswapCoinReceive",
                signature: "claimZswapCoinReceive(note: Bytes<32>)",
                snippet: "claimZswapCoinReceive(${1:note})",
                documentation: "Claim a Zswap coin receive commitment.",
            },
            BuiltinMethod {
                name: "claimZswapCoinSpend",
                signature: "claimZswapCoinSpend(note: Bytes<32>)",
                snippet: "claimZswapCoinSpend(${1:note})",
                documentation: "Claim a Zswap coin spend commitment.",
            },
            BuiltinMethod {
                name: "claimZswapNullifier",
                signature: "claimZswapNullifier(nul: Bytes<32>)",
                snippet: "claimZswapNullifier(${1:nul})",
                documentation: "Claim a Zswap nullifier.",
            },
        ],
        _ => &[],
    }
}

/// Find a specific built-in method by type and method name.
pub fn find_method_by_name<'a>(type_name: &str, method_name: &str) -> Option<&'a BuiltinMethod> {
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
}
