fn main() {
    let grammar = "vendor/compact-tree-sitter";
    let parser = format!("{grammar}/parser.c");

    let mut compiler = cc::Build::new();
    compiler
        .std("c11")
        .include(grammar)
        .file(&parser)
        .warnings(false);

    #[cfg(target_env = "msvc")]
    compiler.flag("-utf-8");

    compiler.compile("tree-sitter-compact");

    println!("cargo:rerun-if-changed={parser}");
    println!("cargo:rerun-if-changed={grammar}/tree_sitter/parser.h");
}
