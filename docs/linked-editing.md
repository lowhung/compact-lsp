# Linked editing

Linked editing lets an editor mirror a small text change across syntax ranges
that must stay identical. `compact-lsp` supports this for Compact import
prefixes:

```compact
import "./Utils" prefix Utils_;

circuit total(): Field {
    return Utils_add(1, 2) + Utils_subtract(3, 1);
}
```

Placing the cursor inside `Utils_` in the import or either call links all three
prefix ranges. Editing the prefix in one range updates the others.

## What the server returns

The `textDocument/linkedEditingRange` handler parses the current open document
and returns:

- The identifier range in the `prefix` declaration.
- The identical prefix segment at the start of each complete direct call.
- The word pattern `[A-Za-z_][A-Za-z0-9_]*`, matching Compact identifiers.

All positions are LSP UTF-16 positions. The ranges have identical content and
length and never overlap, as required by the protocol.

## Safety boundary

Linked editing is intentionally narrower than Rename Symbol. It returns no
result when syntax alone cannot prove that mirroring an edit is safe:

- The prefix is declared more than once.
- A call matches more than one declared prefix.
- A local circuit, external circuit, or witness has the complete rendered call
  name.
- The cursor is in the call suffix instead of the prefix.
- The construct is a member call or contains incomplete syntax.
- The prefix has no complete direct call in the document.

These cases can still use the normal rename workflow when the server can
resolve the symbol. Returning no linked ranges prevents an editor from changing
unrelated text while a document is incomplete or ambiguous.

## Zed

Zed enables linked edits by default. If they have been disabled, restore the
setting in global settings or `.zed/settings.json`:

```json
{
  "linked_edits": true
}
```

To exercise a local server build, install
[`compact-zed`](https://github.com/lowhung/compact-zed) as a development
extension and set `lsp.compact.binary.path` to the absolute path of
`target/release/compact-lsp`. Open the example above, place the cursor within a
`Utils_` prefix, and type or delete a character. Every linked prefix should
change in the same edit.

Use `zed: open log` and `dev: open language server logs` if the server does not
start or the request is not sent.

## VS Code

Enable the built-in editor setting:

```json
{
  "editor.linkedEditing": true
}
```

Open the example above and edit inside a `Utils_` prefix. VS Code also exposes
the `Start Linked Editing` command for an explicit session. Press Escape to
leave linked-editing mode.

This feature is client-controlled: advertising the LSP capability does not
override an editor setting that disables linked edits.
