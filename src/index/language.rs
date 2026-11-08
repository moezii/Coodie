use std::borrow::Cow;
use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};
use tree_sitter::Language;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SourceLanguage {
    id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParserKind {
    Generic,
    Bash,
    C,
    Cpp,
    CSharp,
    Css,
    Go,
    Html,
    Java,
    Json,
    Kotlin,
    OCaml,
    Php,
    Python,
    R,
    Ruby,
    Rust,
    Scala,
    Swift,
    Toml,
    JavaScript,
    TypeScript,
    Tsx,
}

#[derive(Debug, Clone, Copy)]
pub struct LanguageSpec {
    pub id: &'static str,
    pub name: &'static str,
    pub extensions: &'static [&'static str],
    pub filenames: &'static [&'static str],
    pub shebang_markers: &'static [&'static str],
    pub parser: ParserKind,
}

impl SourceLanguage {
    pub fn new(id: impl AsRef<str>) -> Option<Self> {
        let normalized = normalize_language_id(id.as_ref())?;
        if language_spec(normalized.as_ref()).is_some() {
            Some(Self {
                id: normalized.into_owned(),
            })
        } else {
            None
        }
    }

    pub fn from_path(path: &Path) -> Option<Self> {
        Self::from_path_and_source(path, &[])
    }

    pub fn from_path_and_source(path: &Path, source_prefix: &[u8]) -> Option<Self> {
        detect_by_filename(path)
            .or_else(|| detect_by_extension(path))
            .or_else(|| detect_by_shebang(source_prefix))
            .or_else(|| detect_by_modeline(source_prefix))
            .or_else(|| detect_by_syntax_fingerprint(source_prefix))
            .map(Self::from_spec)
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn display_name(&self) -> &str {
        self.spec()
            .map(|spec| spec.name)
            .unwrap_or(self.id.as_str())
    }

    pub fn cache_label(&self) -> &str {
        &self.id
    }

    pub fn tree_sitter_language(&self) -> Option<Language> {
        match self.parser_kind() {
            ParserKind::Bash => Some(tree_sitter_bash::LANGUAGE.into()),
            ParserKind::C => Some(tree_sitter_c::LANGUAGE.into()),
            ParserKind::Cpp => Some(tree_sitter_cpp::LANGUAGE.into()),
            ParserKind::CSharp => Some(tree_sitter_c_sharp::LANGUAGE.into()),
            ParserKind::Css => Some(tree_sitter_css::LANGUAGE.into()),
            ParserKind::Go => Some(tree_sitter_go::LANGUAGE.into()),
            ParserKind::Html => Some(tree_sitter_html::LANGUAGE.into()),
            ParserKind::Java => Some(tree_sitter_java::LANGUAGE.into()),
            ParserKind::Json => Some(tree_sitter_json::LANGUAGE.into()),
            ParserKind::Kotlin => Some(tree_sitter_kotlin_ng::LANGUAGE.into()),
            ParserKind::OCaml => Some(tree_sitter_ocaml::LANGUAGE_OCAML.into()),
            ParserKind::Php => Some(tree_sitter_php::LANGUAGE_PHP.into()),
            ParserKind::Python => Some(tree_sitter_python::LANGUAGE.into()),
            ParserKind::R => Some(tree_sitter_r::LANGUAGE.into()),
            ParserKind::Ruby => Some(tree_sitter_ruby::LANGUAGE.into()),
            ParserKind::Rust => Some(tree_sitter_rust::LANGUAGE.into()),
            ParserKind::Scala => Some(tree_sitter_scala::LANGUAGE.into()),
            ParserKind::Swift => Some(tree_sitter_swift::LANGUAGE.into()),
            ParserKind::Toml => Some(tree_sitter_toml_ng::LANGUAGE.into()),
            ParserKind::JavaScript => Some(tree_sitter_javascript::LANGUAGE.into()),
            ParserKind::TypeScript => Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
            ParserKind::Tsx => Some(tree_sitter_typescript::LANGUAGE_TSX.into()),
            ParserKind::Generic => None,
        }
    }


    /// A `.h` header, which C and C++ both use and the extension alone cannot
    /// settle. [`SourceLanguage::from_path`] reads it as C; callers that can see
    /// the rest of the repository, or the header's content, refine it with
    /// [`SourceLanguage::refine_header`].
    pub fn is_ambiguous_header(&self, path: &Path) -> bool {
        self.id == "c"
            && path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("h"))
    }

    /// Settle whether an ambiguous `.h` header is C or C++.
    ///
    /// Read as C, a C++ header loses every class, namespace, template and
    /// out-of-class method, which then look like nothing at all. The header is
    /// C++ when the repository's own sources are all C++ (`cpp_only_repository`)
    /// or when its content uses syntax C does not have.
    pub fn refine_header(self, path: &Path, source: &[u8], cpp_only_repository: bool) -> Self {
        if !self.is_ambiguous_header(path) {
            return self;
        }
        if cpp_only_repository || looks_like_cpp_header(source) {
            Self {
                id: "cpp".to_owned(),
            }
        } else {
            self
        }
    }

    fn parser_kind(&self) -> ParserKind {
        self.spec()
            .map(|spec| spec.parser)
            .unwrap_or(ParserKind::Generic)
    }

    fn from_spec(spec: &'static LanguageSpec) -> Self {
        Self {
            id: spec.id.to_owned(),
        }
    }

    fn spec(&self) -> Option<&'static LanguageSpec> {
        language_spec(&self.id)
    }
}

impl fmt::Display for SourceLanguage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.display_name())
    }
}

pub fn language_specs() -> &'static [LanguageSpec] {
    LANGUAGE_SPECS
}

pub fn language_spec(id: &str) -> Option<&'static LanguageSpec> {
    let id = normalize_language_id(id)?;
    LANGUAGE_SPECS.iter().find(|spec| spec.id == id.as_ref())
}

fn detect_by_filename(path: &Path) -> Option<&'static LanguageSpec> {
    let file_name = path.file_name()?.to_str()?.to_ascii_lowercase();
    LANGUAGE_SPECS
        .iter()
        .find(|spec| spec.filenames.iter().any(|name| *name == file_name))
}

fn detect_by_extension(path: &Path) -> Option<&'static LanguageSpec> {
    let file_name = path.file_name()?.to_str()?.to_ascii_lowercase();
    let extension = if file_name.ends_with(".d.ts") {
        "d.ts"
    } else {
        path.extension()?.to_str()?
    }
    .to_ascii_lowercase();

    LANGUAGE_SPECS.iter().find(|spec| {
        spec.extensions
            .iter()
            .any(|candidate| *candidate == extension)
    })
}

fn detect_by_shebang(source_prefix: &[u8]) -> Option<&'static LanguageSpec> {
    let text = String::from_utf8_lossy(source_prefix);
    let first_line = text.lines().next()?.trim().to_ascii_lowercase();
    if !first_line.starts_with("#!") {
        return None;
    }

    LANGUAGE_SPECS.iter().find(|spec| {
        spec.shebang_markers
            .iter()
            .any(|marker| first_line.contains(marker))
    })
}

fn detect_by_modeline(source_prefix: &[u8]) -> Option<&'static LanguageSpec> {
    let text = String::from_utf8_lossy(source_prefix).to_ascii_lowercase();
    for line in text.lines().take(5) {
        for spec in LANGUAGE_SPECS {
            if line.contains(&format!("filetype={}", spec.id))
                || line.contains(&format!("ft={}", spec.id))
                || line.contains(&format!("mode: {}", spec.id))
                || line.contains(&format!("language: {}", spec.id))
            {
                return Some(spec);
            }
        }
    }
    None
}

/// Does a header use syntax that only C++ has?
///
/// Only constructs C cannot parse count: a namespace, a class or template
/// declaration, an access specifier, a scope operator, or a standard header
/// included without `.h`. `extern "C"` does not count, because C headers wrap
/// themselves in it for C++ callers. Comments, string literals and C23
/// attributes (`[[gnu::pure]]`) are skipped so a stray `::` there decides
/// nothing.
pub fn looks_like_cpp_header(source: &[u8]) -> bool {
    let code = strip_comments_and_strings(source);
    for raw_line in code.lines() {
        let line = raw_line.trim();
        if line.starts_with('#') {
            // `#include <vector>`: C standard headers all end in `.h`.
            if let Some(rest) = line.strip_prefix('#').map(str::trim_start) {
                if let Some(target) = rest.strip_prefix("include") {
                    let target = target.trim();
                    if let Some(name) = target.strip_prefix('<').and_then(|t| t.split('>').next()) {
                        if !name.is_empty() && !name.contains('.') {
                            return true;
                        }
                    }
                }
            }
            continue;
        }
        if line.contains("[[") {
            continue;
        }
        let words: Vec<&str> = line
            .split(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
            .filter(|word| !word.is_empty())
            .collect();
        let first = words.first().copied().unwrap_or("");
        if ["public:", "private:", "protected:"]
            .iter()
            .any(|specifier| line.starts_with(specifier))
        {
            return true;
        }
        if first == "namespace" && (words.len() >= 2 || line.contains('{')) {
            return true;
        }
        if first == "template" && line["template".len()..].trim_start().starts_with('<') {
            return true;
        }
        if (first == "class" || words.starts_with(&["enum", "class"]))
            && words.len() >= 2
            && !line.contains('=')
            && !line.contains('(')
        {
            return true;
        }
        if line.contains("::") {
            return true;
        }
    }
    false
}

/// Replace comments and string/char literals with spaces, keeping newlines.
pub(crate) fn strip_comments_and_strings(source: &[u8]) -> String {
    let text = String::from_utf8_lossy(source);
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(current) = chars.next() {
        match current {
            '/' if chars.peek() == Some(&'/') => {
                for next in chars.by_ref() {
                    if next == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut previous = ' ';
                for next in chars.by_ref() {
                    if next == '\n' {
                        out.push('\n');
                    }
                    if previous == '*' && next == '/' {
                        break;
                    }
                    previous = next;
                }
                out.push(' ');
            }
            '"' | '\'' => {
                let quote = current;
                let mut escaped = false;
                for next in chars.by_ref() {
                    if next == '\n' {
                        out.push('\n');
                        break;
                    }
                    if escaped {
                        escaped = false;
                    } else if next == '\\' {
                        escaped = true;
                    } else if next == quote {
                        break;
                    }
                }
                out.push(' ');
            }
            other => out.push(other),
        }
    }
    out
}

fn detect_by_syntax_fingerprint(source_prefix: &[u8]) -> Option<&'static LanguageSpec> {
    let text = String::from_utf8_lossy(source_prefix);
    let lower = text.to_ascii_lowercase();
    let trimmed = text.trim_start();

    if trimmed.starts_with("<?php") {
        return language_spec("php");
    }
    if trimmed.starts_with("<!doctype html") || trimmed.starts_with("<html") {
        return language_spec("html");
    }
    if lower.contains("pragma solidity") {
        return language_spec("solidity");
    }
    if lower.contains("terraform {") || lower.contains("provider \"") {
        return language_spec("terraform");
    }
    if lower.contains("package main") && lower.contains("func main(") {
        return language_spec("go");
    }
    if lower.contains("using system;") || lower.contains("namespace ") && lower.contains("class ") {
        return language_spec("csharp");
    }
    if lower.contains("fn main()") || lower.contains("use std::") {
        return language_spec("rust");
    }

    None
}

fn normalize_language_id(id: &str) -> Option<Cow<'_, str>> {
    let id = id.trim();
    if id.is_empty() {
        return None;
    }

    let lowered = id.to_ascii_lowercase();
    let canonical = match lowered.as_str() {
        "c++" | "cc" | "cxx" | "hpp" | "hxx" => "cpp",
        "c#" | "cs" => "csharp",
        "f#" | "fs" => "fsharp",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "ts" | "mts" | "cts" | "d.ts" => "typescript",
        "py" | "pyw" => "python",
        "rb" => "ruby",
        "rs" => "rust",
        "sh" | "zsh" | "ksh" => "shell",
        "ps1" => "powershell",
        "yml" => "yaml",
        "kt" | "kts" => "kotlin",
        "golang" => "go",
        "objc" => "objective-c",
        "tf" | "hcl" => "terraform",
        "vbnet" => "visual-basic",
        "md" => "markdown",
        _ => return Some(Cow::Owned(lowered)),
    };
    Some(Cow::Borrowed(canonical))
}

const LANGUAGE_SPECS: &[LanguageSpec] = &[
    LanguageSpec {
        id: "ada",
        name: "Ada",
        extensions: &["adb", "ads"],
        filenames: &[],
        shebang_markers: &[],
        parser: ParserKind::Generic,
    },
    LanguageSpec {
        id: "apex",
        name: "Apex",
        extensions: &["cls", "trigger"],
        filenames: &[],
        shebang_markers: &[],
        parser: ParserKind::Generic,
    },
    LanguageSpec {
        id: "assembly",
        name: "Assembly",
        extensions: &["asm", "s"],
        filenames: &[],
        shebang_markers: &[],
        parser: ParserKind::Generic,
    },
    LanguageSpec {
        id: "batch",
        name: "Batch",
        extensions: &["bat", "cmd"],
        filenames: &[],
        shebang_markers: &[],
        parser: ParserKind::Generic,
    },
    LanguageSpec {
        id: "c",
        name: "C",
        extensions: &["c", "h"],
        filenames: &[],
        shebang_markers: &[],
        parser: ParserKind::C,
    },
    LanguageSpec {
        id: "clojure",
        name: "Clojure",
        extensions: &["clj", "cljs", "cljc", "edn"],
        filenames: &[],
        shebang_markers: &["clojure", "bb"],
        parser: ParserKind::Generic,
    },
    LanguageSpec {
        id: "cmake",
        name: "CMake",
        extensions: &["cmake"],
        filenames: &["cmakelists.txt"],
        shebang_markers: &[],
        parser: ParserKind::Generic,
    },
    LanguageSpec {
        id: "cobol",
        name: "COBOL",
        extensions: &["cob", "cbl", "cpy"],
        filenames: &[],
        shebang_markers: &[],
        parser: ParserKind::Generic,
    },
    LanguageSpec {
        id: "coffeescript",
        name: "CoffeeScript",
        extensions: &["coffee"],
        filenames: &[],
        shebang_markers: &["coffee"],
        parser: ParserKind::Generic,
    },
    LanguageSpec {
        id: "cpp",
        name: "C++",
        // `.inl`/`.ipp`/`.tcc`/`.tpp` hold template and inline definitions
        // that a header includes; they are ordinary C++.
        extensions: &[
            "cpp", "cc", "cxx", "c++", "hpp", "hh", "hxx", "h++", "inl", "ipp", "tcc", "tpp",
        ],
        filenames: &[],
        shebang_markers: &[],
        parser: ParserKind::Cpp,
    },
    LanguageSpec {
        id: "crystal",
        name: "Crystal",
        extensions: &["cr"],
        filenames: &[],
        shebang_markers: &["crystal"],
        parser: ParserKind::Generic,
    },
    LanguageSpec {
        id: "csharp",
        name: "C#",
        extensions: &["cs", "csx"],
        filenames: &[],
        shebang_markers: &[],
        parser: ParserKind::CSharp,
    },
    LanguageSpec {
        id: "css",
        name: "CSS",
        extensions: &["css", "scss", "sass", "less"],
        filenames: &[],
        shebang_markers: &[],
        parser: ParserKind::Css,
    },
    // C-family dialects with no grammar here. They are never a source of
    // symbols, but they call into the C and C++ code that is, so every word in
    // them is read as a possible reference (see `audit::cfamily`).
    LanguageSpec {
        id: "cuda",
        name: "CUDA",
        extensions: &["cu", "cuh"],
        filenames: &[],
        shebang_markers: &[],
        parser: ParserKind::Generic,
    },
    LanguageSpec {
        id: "shader",
        name: "GPU kernel / shader",
        extensions: &[
            "hlsl", "hlsli", "glsl", "vert", "frag", "comp", "geom", "metal", "ispc", "opencl",
        ],
        filenames: &[],
        shebang_markers: &[],
        parser: ParserKind::Generic,
    },
    LanguageSpec {
        id: "c-include",
        name: "C/C++ include fragment",
        extensions: &["inc"],
        filenames: &[],
        shebang_markers: &[],
        parser: ParserKind::Generic,
    },
    LanguageSpec {
        id: "dart",
        name: "Dart",
        extensions: &["dart"],
        filenames: &[],
        shebang_markers: &["dart"],
        parser: ParserKind::Generic,
    },
    LanguageSpec {
        id: "dockerfile",
        name: "Dockerfile",
        extensions: &["dockerfile"],
        filenames: &["dockerfile", "containerfile"],
        shebang_markers: &[],
        parser: ParserKind::Generic,
    },
    LanguageSpec {
        id: "elixir",
        name: "Elixir",
        extensions: &["ex", "exs"],
        filenames: &[],
        shebang_markers: &["elixir", "mix"],
        parser: ParserKind::Generic,
    },
    LanguageSpec {
        id: "elm",
        name: "Elm",
        extensions: &["elm"],
        filenames: &[],
        shebang_markers: &[],
        parser: ParserKind::Generic,
    },
    LanguageSpec {
        id: "erlang",
        name: "Erlang",
        extensions: &["erl", "hrl"],
        filenames: &[],
        shebang_markers: &["escript"],
        parser: ParserKind::Generic,
    },
    LanguageSpec {
        id: "fsharp",
        name: "F#",
        extensions: &["fs", "fsi", "fsx"],
        filenames: &[],
        shebang_markers: &[],
        parser: ParserKind::Generic,
    },
    LanguageSpec {
        id: "go",
        name: "Go",
        extensions: &["go"],
        filenames: &[],
        shebang_markers: &[],
        parser: ParserKind::Go,
    },
    LanguageSpec {
        id: "graphql",
        name: "GraphQL",
        extensions: &["graphql", "gql"],
        filenames: &[],
        shebang_markers: &[],
        parser: ParserKind::Generic,
    },
    LanguageSpec {
        id: "groovy",
        name: "Groovy",
        extensions: &["groovy", "gradle"],
        filenames: &[],
        shebang_markers: &["groovy"],
        parser: ParserKind::Generic,
    },
    LanguageSpec {
        id: "haskell",
        name: "Haskell",
        extensions: &["hs", "lhs"],
        filenames: &[],
        shebang_markers: &["runhaskell", "runghc"],
        parser: ParserKind::Generic,
    },
    LanguageSpec {
        id: "html",
        name: "HTML",
        extensions: &["html", "htm"],
        filenames: &[],
        shebang_markers: &[],
        parser: ParserKind::Html,
    },
    LanguageSpec {
        id: "java",
        name: "Java",
        extensions: &["java"],
        filenames: &[],
        shebang_markers: &[],
        parser: ParserKind::Java,
    },
    LanguageSpec {
        id: "javascript",
        name: "JavaScript",
        extensions: &["js", "jsx", "mjs", "cjs"],
        filenames: &[],
        shebang_markers: &["node", "deno"],
        parser: ParserKind::JavaScript,
    },
    LanguageSpec {
        id: "json",
        name: "JSON",
        extensions: &["json", "jsonc", "json5"],
        filenames: &[],
        shebang_markers: &[],
        parser: ParserKind::Json,
    },
    LanguageSpec {
        id: "julia",
        name: "Julia",
        extensions: &["jl"],
        filenames: &[],
        shebang_markers: &["julia"],
        parser: ParserKind::Generic,
    },
    LanguageSpec {
        id: "kotlin",
        name: "Kotlin",
        extensions: &["kt", "kts"],
        filenames: &[],
        shebang_markers: &["kotlin"],
        parser: ParserKind::Kotlin,
    },
    LanguageSpec {
        id: "lua",
        name: "Lua",
        extensions: &["lua"],
        filenames: &[],
        shebang_markers: &["lua"],
        parser: ParserKind::Generic,
    },
    LanguageSpec {
        id: "make",
        name: "Make",
        extensions: &["mk", "mak"],
        filenames: &["makefile", "gnumakefile"],
        shebang_markers: &[],
        parser: ParserKind::Generic,
    },
    LanguageSpec {
        id: "markdown",
        name: "Markdown",
        extensions: &["md", "markdown", "mdx"],
        filenames: &[],
        shebang_markers: &[],
        parser: ParserKind::Generic,
    },
    LanguageSpec {
        id: "matlab",
        name: "MATLAB",
        extensions: &["m"],
        filenames: &[],
        shebang_markers: &[],
        parser: ParserKind::Generic,
    },
    LanguageSpec {
        id: "nim",
        name: "Nim",
        extensions: &["nim", "nims"],
        filenames: &[],
        shebang_markers: &["nim"],
        parser: ParserKind::Generic,
    },
    LanguageSpec {
        id: "nix",
        name: "Nix",
        extensions: &["nix"],
        filenames: &[],
        shebang_markers: &["nix-shell"],
        parser: ParserKind::Generic,
    },
    LanguageSpec {
        id: "objective-c",
        name: "Objective-C",
        extensions: &["mm"],
        filenames: &[],
        shebang_markers: &[],
        parser: ParserKind::Generic,
    },
    LanguageSpec {
        id: "ocaml",
        name: "OCaml",
        extensions: &["ml", "mli"],
        filenames: &[],
        shebang_markers: &["ocaml"],
        parser: ParserKind::OCaml,
    },
    LanguageSpec {
        id: "perl",
        name: "Perl",
        extensions: &["pl", "pm", "t"],
        filenames: &[],
        shebang_markers: &["perl"],
        parser: ParserKind::Generic,
    },
    LanguageSpec {
        id: "php",
        name: "PHP",
        extensions: &["php", "php3", "php4", "php5", "phtml"],
        filenames: &[],
        shebang_markers: &["php"],
        parser: ParserKind::Php,
    },
    LanguageSpec {
        id: "powershell",
        name: "PowerShell",
        extensions: &["ps1", "psm1", "psd1"],
        filenames: &[],
        shebang_markers: &["pwsh", "powershell"],
        parser: ParserKind::Generic,
    },
    LanguageSpec {
        id: "protobuf",
        name: "Protocol Buffers",
        extensions: &["proto"],
        filenames: &[],
        shebang_markers: &[],
        parser: ParserKind::Generic,
    },
    LanguageSpec {
        id: "python",
        name: "Python",
        extensions: &["py", "pyw", "pyi"],
        filenames: &[],
        shebang_markers: &["python", "python3"],
        parser: ParserKind::Python,
    },
    LanguageSpec {
        id: "r",
        name: "R",
        extensions: &["r", "rmd"],
        filenames: &[],
        shebang_markers: &["rscript"],
        parser: ParserKind::R,
    },
    LanguageSpec {
        id: "racket",
        name: "Racket",
        extensions: &["rkt"],
        filenames: &[],
        shebang_markers: &["racket"],
        parser: ParserKind::Generic,
    },
    LanguageSpec {
        id: "ruby",
        name: "Ruby",
        extensions: &["rb", "rake"],
        filenames: &["gemfile", "rakefile"],
        shebang_markers: &["ruby"],
        parser: ParserKind::Ruby,
    },
    LanguageSpec {
        id: "rust",
        name: "Rust",
        extensions: &["rs"],
        filenames: &[],
        shebang_markers: &[],
        parser: ParserKind::Rust,
    },
    LanguageSpec {
        id: "scala",
        name: "Scala",
        extensions: &["scala", "sc"],
        filenames: &[],
        shebang_markers: &["scala"],
        parser: ParserKind::Scala,
    },
    LanguageSpec {
        id: "scheme",
        name: "Scheme",
        extensions: &["scm", "ss"],
        filenames: &[],
        shebang_markers: &["scheme", "guile"],
        parser: ParserKind::Generic,
    },
    LanguageSpec {
        id: "shell",
        name: "Shell",
        extensions: &["sh", "bash", "zsh", "ksh"],
        filenames: &[".bashrc", ".zshrc", ".profile", ".bash_profile"],
        shebang_markers: &["sh", "bash", "zsh", "ksh"],
        parser: ParserKind::Bash,
    },
    LanguageSpec {
        id: "solidity",
        name: "Solidity",
        extensions: &["sol"],
        filenames: &[],
        shebang_markers: &[],
        parser: ParserKind::Generic,
    },
    LanguageSpec {
        id: "sql",
        name: "SQL",
        extensions: &["sql"],
        filenames: &[],
        shebang_markers: &[],
        parser: ParserKind::Generic,
    },
    LanguageSpec {
        id: "svelte",
        name: "Svelte",
        extensions: &["svelte"],
        filenames: &[],
        shebang_markers: &[],
        parser: ParserKind::Generic,
    },
    LanguageSpec {
        id: "swift",
        name: "Swift",
        extensions: &["swift"],
        filenames: &[],
        shebang_markers: &["swift"],
        parser: ParserKind::Swift,
    },
    LanguageSpec {
        id: "terraform",
        name: "Terraform",
        extensions: &["tf", "tfvars", "hcl"],
        filenames: &[],
        shebang_markers: &[],
        parser: ParserKind::Generic,
    },
    LanguageSpec {
        id: "toml",
        name: "TOML",
        extensions: &["toml"],
        filenames: &["cargo.lock"],
        shebang_markers: &[],
        parser: ParserKind::Toml,
    },
    LanguageSpec {
        id: "tsx",
        name: "TSX",
        extensions: &["tsx"],
        filenames: &[],
        shebang_markers: &[],
        parser: ParserKind::Tsx,
    },
    LanguageSpec {
        id: "typescript",
        name: "TypeScript",
        extensions: &["ts", "mts", "cts", "d.ts"],
        filenames: &[],
        shebang_markers: &["ts-node", "tsx"],
        parser: ParserKind::TypeScript,
    },
    LanguageSpec {
        id: "visual-basic",
        name: "Visual Basic",
        extensions: &["vb", "vbs"],
        filenames: &[],
        shebang_markers: &[],
        parser: ParserKind::Generic,
    },
    LanguageSpec {
        id: "vue",
        name: "Vue",
        extensions: &["vue"],
        filenames: &[],
        shebang_markers: &[],
        parser: ParserKind::Generic,
    },
    LanguageSpec {
        id: "xml",
        name: "XML",
        extensions: &["xml", "xsd", "xsl", "xslt", "svg"],
        filenames: &[],
        shebang_markers: &[],
        parser: ParserKind::Generic,
    },
    LanguageSpec {
        id: "yaml",
        name: "YAML",
        extensions: &["yaml", "yml"],
        filenames: &[],
        shebang_markers: &[],
        parser: ParserKind::Generic,
    },
    LanguageSpec {
        id: "zig",
        name: "Zig",
        extensions: &["zig", "zon"],
        filenames: &[],
        shebang_markers: &[],
        parser: ParserKind::Generic,
    },
];

#[cfg(test)]
mod tests {
    use super::{language_specs, SourceLanguage};

    #[test]
    fn cpp_headers_are_told_apart_from_c_headers_by_syntax() {
        use super::looks_like_cpp_header;
        for cpp in [
            "namespace Stockfish {\nint x;\n}\n",
            "class Position {\n public:\n  int key() const;\n};\n",
            "template <typename T>\nT clamp(T v);\n",
            "template<int N> struct Table;\n",
            "#include <vector>\nint f();\n",
            "inline int f() { return std::max(1, 2); }\n",
            "struct S {\nprivate:\n  int x;\n};\n",
            "enum class Color { White, Black };\n",
        ] {
            assert!(looks_like_cpp_header(cpp.as_bytes()), "should be C++: {cpp}");
        }
        for c in [
            "#include <stdio.h>\nint helper(int x);\n",
            "#ifdef __cplusplus\nextern \"C\" {\n#endif\nint f(void);\n",
            "/* uses std::vector internally */\nint f(void);\n",
            "const char* s = \"a::b\";\n",
            "int class_count; int namespace;\n",
            "[[gnu::pure]] int f(void);\n",
        ] {
            assert!(!looks_like_cpp_header(c.as_bytes()), "should stay C: {c}");
        }
        let header = SourceLanguage::from_path("a/b.h".as_ref()).unwrap();
        assert_eq!(header.cache_label(), "c");
        assert_eq!(
            header
                .clone()
                .refine_header("a/b.h".as_ref(), b"int f(void);", true)
                .cache_label(),
            "cpp"
        );
        assert_eq!(
            header
                .refine_header("a/b.h".as_ref(), b"int f(void);", false)
                .cache_label(),
            "c"
        );
        // A `.c` file is never reinterpreted.
        let source = SourceLanguage::from_path("a/b.c".as_ref()).unwrap();
        assert_eq!(
            source
                .refine_header("a/b.c".as_ref(), b"namespace x {}", true)
                .cache_label(),
            "c"
        );
    }

    #[test]
    fn detects_core_and_modern_source_extensions() {
        assert_eq!(
            SourceLanguage::from_path("app.py".as_ref())
                .unwrap()
                .cache_label(),
            "python"
        );
        assert_eq!(
            SourceLanguage::from_path("client.jsx".as_ref())
                .unwrap()
                .cache_label(),
            "javascript"
        );
        assert_eq!(
            SourceLanguage::from_path("types.ts".as_ref())
                .unwrap()
                .cache_label(),
            "typescript"
        );
        assert_eq!(
            SourceLanguage::from_path("view.tsx".as_ref())
                .unwrap()
                .cache_label(),
            "tsx"
        );
        assert_eq!(
            SourceLanguage::from_path("main.rs".as_ref())
                .unwrap()
                .cache_label(),
            "rust"
        );
        assert_eq!(
            SourceLanguage::from_path("service.go".as_ref())
                .unwrap()
                .cache_label(),
            "go"
        );
        assert_eq!(
            SourceLanguage::from_path("Dockerfile".as_ref())
                .unwrap()
                .cache_label(),
            "dockerfile"
        );
    }

    #[test]
    fn detects_language_from_shebang_and_syntax_fingerprint() {
        let shell = SourceLanguage::from_path_and_source(
            "deploy".as_ref(),
            b"#!/usr/bin/env bash\nset -e\n",
        )
        .unwrap();
        assert_eq!(shell.cache_label(), "shell");

        let php =
            SourceLanguage::from_path_and_source("index".as_ref(), b"<?php\nfunction boot() {}")
                .unwrap();
        assert_eq!(php.cache_label(), "php");

        let rust =
            SourceLanguage::from_path_and_source("program".as_ref(), b"fn main() {}\n").unwrap();
        assert_eq!(rust.cache_label(), "rust");
    }

    #[test]
    fn registry_is_broad_enough_to_not_be_a_four_language_ceiling() {
        assert!(language_specs().len() >= 50);
    }
}
