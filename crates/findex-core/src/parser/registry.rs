use std::collections::HashMap;
use std::sync::LazyLock;
use tree_sitter::Language;

/// Configuration for one tree-sitter-backed language.
///
/// The query string must follow the convention:
///   - definition captures: `@<prefix>.def` and `@<prefix>.name`
///   - call captures: `@call.name`
///   - reference captures: `@ref.name`
///
/// `kind_map` translates the capture prefix into a `Symbol.kind`.
pub struct LanguageConfig {
    pub name: &'static str,
    pub extensions: &'static [&'static str],
    pub language: Language,
    pub query: &'static str,
    pub kind_map: &'static [(&'static str, &'static str)],
}

const RUST_QUERY: &str = include_str!("../../queries/rust.scm");
const PYTHON_QUERY: &str = include_str!("../../queries/python.scm");
const HTML_QUERY: &str = include_str!("../../queries/html.scm");
const CSS_QUERY: &str = include_str!("../../queries/css.scm");
const DART_QUERY: &str = include_str!("../../queries/dart.scm");

#[cfg(feature = "lang-c")]
const C_QUERY: &str = include_str!("../../queries/c.scm");
#[cfg(feature = "lang-cpp")]
const CPP_QUERY: &str = include_str!("../../queries/cpp.scm");
#[cfg(feature = "lang-go")]
const GO_QUERY: &str = include_str!("../../queries/go.scm");
#[cfg(feature = "lang-java")]
const JAVA_QUERY: &str = include_str!("../../queries/java.scm");
#[cfg(feature = "lang-csharp")]
const CSHARP_QUERY: &str = include_str!("../../queries/csharp.scm");
#[cfg(feature = "lang-ruby")]
const RUBY_QUERY: &str = include_str!("../../queries/ruby.scm");
#[cfg(feature = "lang-php")]
const PHP_QUERY: &str = include_str!("../../queries/php.scm");
#[cfg(feature = "lang-swift")]
const SWIFT_QUERY: &str = include_str!("../../queries/swift.scm");

const DEFAULT_KIND_MAP: &[(&str, &str)] = &[
    ("func", "Function"),
    ("struct", "Struct"),
    ("enum", "Enum"),
    ("trait", "Trait"),
    ("impl", "Impl"),
    ("class", "Class"),
    ("interface", "Interface"),
    ("type", "Type"),
    ("alias", "TypeAlias"),
    ("union", "Union"),
    ("module", "Module"),
    ("namespace", "Namespace"),
    ("method", "Method"),
    ("constructor", "Constructor"),
    ("property", "Property"),
    ("field", "Field"),
    ("record", "Record"),
    ("annotation", "Annotation"),
    ("mixin", "Mixin"),
    ("extension", "Extension"),
    ("protocol", "Protocol"),
    ("variant", "EnumVariant"),
    ("constant", "Constant"),
    ("tag", "Tag"),
    ("css", "Symbol"),
];

#[cfg(feature = "lang-c")]
const C_KIND_MAP: &[(&str, &str)] = &[("func", "Function"), ("struct", "Struct"), ("enum", "Enum")];

#[cfg(feature = "lang-cpp")]
const CPP_KIND_MAP: &[(&str, &str)] = &[
    ("func", "Function"),
    ("struct", "Struct"),
    ("enum", "Enum"),
    ("class", "Class"),
    ("union", "Union"),
    ("alias", "TypeAlias"),
    ("namespace", "Namespace"),
];

#[cfg(feature = "lang-go")]
const GO_KIND_MAP: &[(&str, &str)] = &[
    ("func", "Function"),
    ("type", "Type"),
    ("struct", "Struct"),
    ("interface", "Interface"),
    ("alias", "TypeAlias"),
];

#[cfg(feature = "lang-java")]
const JAVA_KIND_MAP: &[(&str, &str)] = &[
    ("class", "Class"),
    ("interface", "Interface"),
    ("func", "Function"),
    ("method", "Method"),
    ("constructor", "Constructor"),
    ("enum", "Enum"),
    ("record", "Record"),
    ("annotation", "Annotation"),
    ("module", "Module"),
];

fn build_registry() -> HashMap<&'static str, &'static LanguageConfig> {
    let mut reg: HashMap<&'static str, &'static LanguageConfig> = HashMap::new();

    let rust = LanguageConfig {
        name: "rust",
        extensions: &["rs"],
        language: tree_sitter_rust::LANGUAGE.into(),
        query: RUST_QUERY,
        kind_map: DEFAULT_KIND_MAP,
    };
    let python = LanguageConfig {
        name: "python",
        extensions: &["py"],
        language: tree_sitter_python::LANGUAGE.into(),
        query: PYTHON_QUERY,
        kind_map: DEFAULT_KIND_MAP,
    };
    let html = LanguageConfig {
        name: "html",
        extensions: &["html", "htm"],
        language: tree_sitter_html::LANGUAGE.into(),
        query: HTML_QUERY,
        kind_map: DEFAULT_KIND_MAP,
    };
    let css = LanguageConfig {
        name: "css",
        extensions: &["css"],
        language: tree_sitter_css::LANGUAGE.into(),
        query: CSS_QUERY,
        kind_map: DEFAULT_KIND_MAP,
    };
    let dart = LanguageConfig {
        name: "dart",
        extensions: &["dart"],
        language: tree_sitter_dart::language(),
        query: DART_QUERY,
        kind_map: DEFAULT_KIND_MAP,
    };

    #[cfg(feature = "lang-c")]
    let c_cfg = LanguageConfig {
        name: "c",
        extensions: &["c", "h"],
        language: tree_sitter_c::LANGUAGE.into(),
        query: C_QUERY,
        kind_map: C_KIND_MAP,
    };
    #[cfg(feature = "lang-cpp")]
    let cpp_cfg = LanguageConfig {
        name: "cpp",
        extensions: &["cpp", "cc", "cxx", "hpp", "hxx"],
        language: tree_sitter_cpp::LANGUAGE.into(),
        query: CPP_QUERY,
        kind_map: CPP_KIND_MAP,
    };
    #[cfg(feature = "lang-go")]
    let go_cfg = LanguageConfig {
        name: "go",
        extensions: &["go"],
        language: tree_sitter_go::LANGUAGE.into(),
        query: GO_QUERY,
        kind_map: GO_KIND_MAP,
    };
    #[cfg(feature = "lang-java")]
    let java_cfg = LanguageConfig {
        name: "java",
        extensions: &["java"],
        language: tree_sitter_java::LANGUAGE.into(),
        query: JAVA_QUERY,
        kind_map: JAVA_KIND_MAP,
    };
    #[cfg(feature = "lang-csharp")]
    let csharp_cfg = LanguageConfig {
        name: "csharp",
        extensions: &["cs"],
        language: tree_sitter_c_sharp::LANGUAGE.into(),
        query: CSHARP_QUERY,
        kind_map: DEFAULT_KIND_MAP,
    };
    #[cfg(feature = "lang-ruby")]
    let ruby_cfg = LanguageConfig {
        name: "ruby",
        extensions: &["rb", "rake"],
        language: tree_sitter_ruby::LANGUAGE.into(),
        query: RUBY_QUERY,
        kind_map: DEFAULT_KIND_MAP,
    };
    #[cfg(feature = "lang-php")]
    let php_cfg = LanguageConfig {
        name: "php",
        extensions: &["php", "phtml"],
        language: tree_sitter_php::LANGUAGE_PHP.into(),
        query: PHP_QUERY,
        kind_map: DEFAULT_KIND_MAP,
    };
    #[cfg(feature = "lang-swift")]
    let swift_cfg = LanguageConfig {
        name: "swift",
        extensions: &["swift"],
        language: tree_sitter_swift::LANGUAGE.into(),
        query: SWIFT_QUERY,
        kind_map: DEFAULT_KIND_MAP,
    };

    // Leak the configs so they live for the static lifetime of the registry.
    #[allow(unused_mut)]
    let mut configs: Vec<LanguageConfig> = vec![rust, python, html, css, dart];
    #[cfg(feature = "lang-c")]
    configs.push(c_cfg);
    #[cfg(feature = "lang-cpp")]
    configs.push(cpp_cfg);
    #[cfg(feature = "lang-go")]
    configs.push(go_cfg);
    #[cfg(feature = "lang-java")]
    configs.push(java_cfg);
    #[cfg(feature = "lang-csharp")]
    configs.push(csharp_cfg);
    #[cfg(feature = "lang-ruby")]
    configs.push(ruby_cfg);
    #[cfg(feature = "lang-php")]
    configs.push(php_cfg);
    #[cfg(feature = "lang-swift")]
    configs.push(swift_cfg);

    for cfg in configs {
        let cfg: &'static LanguageConfig = Box::leak(Box::new(cfg));
        for ext in cfg.extensions {
            reg.insert(ext, cfg);
        }
    }

    reg
}

pub static REGISTRY: LazyLock<HashMap<&'static str, &'static LanguageConfig>> =
    LazyLock::new(build_registry);

/// Look up the language config for a file extension.
pub fn config_for_extension(ext: &str) -> Option<&'static LanguageConfig> {
    REGISTRY.get(ext).copied()
}

/// Return all supported extensions.
pub fn supported_extensions() -> Vec<&'static str> {
    REGISTRY.keys().copied().collect()
}

/// True if the extension is handled by any registered language.
pub fn is_supported_extension(ext: &str) -> bool {
    REGISTRY.contains_key(ext)
}
