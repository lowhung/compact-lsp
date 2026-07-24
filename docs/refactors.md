# Semantic refactors

`compact-lsp` exposes conservative refactors through
`textDocument/codeAction`. Each action contains a complete `WorkspaceEdit`, so
an editor can show the proposed changes before applying them.

## Extract local value

Select an exact expression inside a Compact circuit:

```compact
circuit calculate(a: Field, b: Field): Field {
    return a + b * 3;
}
```

Choose **Extract to local `extractedValue`**. The action produces:

```compact
circuit calculate(a: Field, b: Field): Field {
    const extractedValue = a + b * 3;
    return extractedValue;
}
```

The generated name is deterministic. If `extractedValue` already appears
anywhere in the document, the server tries `extractedValue2`,
`extractedValue3`, and so on rather than introducing a shadowing collision.

### Safety contract

The action is available only when all of these conditions hold:

- The non-empty selection exactly matches one complete AST expression.
- The selection is on one line inside a circuit.
- The containing statement is a standalone `return` or local `const`.
- The complete statement contains only reviewed, effect-free expression forms.
- The selection does not cross a conditional or short-circuit evaluation
  boundary.
- The syntax tree contains no error or missing node in the statement.
- The insertion point contains only the statement's existing indentation.

Calls, assignments, assertions, disclosures, map/fold expressions, comments
inside the statement, incomplete code, multi-line selections, and
not-yet-reviewed grammar constructs return no action. The whole statement is
checked—not only the selection—because extraction evaluates the new constant
immediately before the statement. This prevents a sibling call or mutation
from making the evaluation order observable. Nested conditional branches and
short-circuit operands are also rejected because hoisting them would make
their evaluation unconditional.

The first implementation intentionally favors a missing action over a
transformation whose semantics cannot be proved from the current syntax-only
analysis.

## How the implementation is divided

`ParserEngine::extract_local_value_plan` owns the semantic safety boundary. It
parses once, resolves an exact UTF-16 selection, checks the containing
statement, generates a collision-free name, and returns an
`ExtractLocalValuePlan`. It never mutates source text.

The parser helpers have narrower responsibilities:

- The `deepest_named_node_with_range` helper resolves wrapper-heavy Tree-sitter
  ranges to the concrete selected expression.
- The `has_expression_context` helper distinguishes expressions from
  identical-looking names in types and declaration patterns.
- The `crosses_deferred_evaluation_boundary` helper prevents conditional
  branches and short-circuit operands from being evaluated eagerly.
- The `is_extract_safe_statement` and `is_extract_safe_subtree` helpers
  implement the explicit effect-free allowlist. New grammar nodes remain
  disabled until reviewed.
- The `collect_source_names` helper prevents the generated local from shadowing
  any spelling already present in the document.

The language-server `code_action` handler respects hierarchical LSP action-kind
filters, asks the analyzer for a plan, and converts a successful plan into two
non-overlapping text edits. A `None` plan simply means the refactor is not
offered.

## Editor use

In Zed, select the expression, open the Code Actions menu, and choose the
extract action. In VS Code, select the expression and use the lightbulb or
**Quick Fix...** menu. If the action is absent, first confirm that the
selection excludes surrounding whitespace, then check the safety restrictions
above.

## Evaluated follow-up refactors

| Refactor | Status | Required safety work |
|----------|--------|----------------------|
| Extract local value | Implemented | Expand the reviewed pure-expression allowlist only with regression tests |
| Extract circuit | Deferred | Resolve free variables, preserve parameter and return types, and classify circuit effects |
| Inline local value | Deferred | Prove scope, use count, precedence, and evaluation-order preservation |
| Import-prefix rewrite | Deferred as a Code Action | Resolve every prefixed symbol form and reject local/import collisions; linked editing already supports interactive prefix changes |

Keeping these transformations independent avoids coupling a proven local
refactor to analysis that the server does not yet have.
