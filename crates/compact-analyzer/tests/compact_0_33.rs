use compact_analyzer::ParserEngine;

const COMPACT_0_33_FIXTURE: &str = include_str!("fixtures/compact_0_33.compact");

#[test]
fn parses_compact_0_33_language_features_without_errors() {
    let mut parser = ParserEngine::new();
    let errors = parser.get_syntax_errors(COMPACT_0_33_FIXTURE);

    assert!(
        errors.is_empty(),
        "expected the Compact 0.33 compatibility fixture to parse without errors: {errors:#?}"
    );
}

#[test]
fn extracts_compact_0_33_contract_and_module_symbols() {
    let mut parser = ParserEngine::new();
    let symbols = parser.document_symbols(COMPACT_0_33_FIXTURE);

    for expected in [
        "Numbers",
        "Calculator",
        "calculator",
        "calculate",
        "verifySignature",
    ] {
        assert!(
            symbols.iter().any(|symbol| symbol.name == expected),
            "expected symbol {expected:?}; found {symbols:#?}"
        );
    }
}
