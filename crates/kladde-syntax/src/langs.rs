//! Language table and lookup.

use std::path::Path;

use crate::code::{st, CodeSpec, Flavor, Str, Vars, BASE};

#[derive(Clone, Copy)]
pub(crate) enum Mode {
    Plain,
    Code(&'static CodeSpec),
    Xml,
    Markdown,
    Csv(u8),
    Diff,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum FoldStyle {
    None,
    Braces,
    /// Braces plus square brackets (JSON arrays).
    Brackets,
    Indent,
    Tags {
        html: bool,
    },
    Headings,
    Sections,
}

/// A supported language.
pub struct Language {
    pub id: &'static str,
    pub name: &'static str,
    /// Lowercase file extensions without dot.
    pub extensions: &'static [&'static str],
    file_names: &'static [&'static str],
    fence_tags: &'static [&'static str],
    line_comment: Option<&'static str>,
    block_comment: Option<(&'static str, &'static str)>,
    pub(crate) mode: Mode,
    pub(crate) fold: FoldStyle,
}

impl Language {
    /// Prefix used by the toggle-comment command.
    pub fn line_comment(&self) -> Option<&'static str> {
        self.line_comment
    }

    /// Delimiters used by the block-comment command.
    pub fn block_comment(&self) -> Option<(&'static str, &'static str)> {
        self.block_comment
    }
}

impl std::fmt::Debug for Language {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Language").field("id", &self.id).finish()
    }
}

impl PartialEq for Language {
    fn eq(&self, other: &Language) -> bool {
        self.id == other.id
    }
}

impl Eq for Language {}

/// All languages; plain text first.
pub fn languages() -> &'static [Language] {
    &LANGUAGES
}

pub fn language_by_id(id: &str) -> Option<&'static Language> {
    LANGUAGES.iter().find(|l| l.id == id)
}

/// Language for a file name or extension; plain text when unknown.
pub fn language_for_path(path: &Path) -> &'static Language {
    let name = path.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    let ext = name.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    let by_name = LANGUAGES.iter().find(|l| l.file_names.contains(&name.as_str()));
    let by_ext = || LANGUAGES.iter().find(|l| !ext.is_empty() && l.extensions.contains(&ext));
    by_name.or_else(by_ext).unwrap_or(&LANGUAGES[0])
}

/// Language for a Markdown fence info string such as `python`, `{r}`, `{.bash}` or `js title="x"`.
pub fn language_for_fence(tag: &str) -> Option<&'static Language> {
    let word = tag
        .trim()
        .trim_start_matches('{')
        .trim_start_matches('.')
        .split(|c: char| c.is_whitespace() || c == ',' || c == '}' || c == '{')
        .next()
        .unwrap_or("")
        .to_lowercase();
    let known =
        |l: &&Language| l.id == word || l.fence_tags.contains(&word.as_str()) || l.extensions.contains(&word.as_str());
    LANGUAGES.iter().find(|l| !word.is_empty() && known(l))
}

pub(crate) fn index_of(lang: &'static Language) -> usize {
    LANGUAGES.iter().position(|l| std::ptr::eq(l, lang)).unwrap_or(0)
}

// ---- string tables ------------------------------------------------------------------------------------------------

const C_STRINGS: &[Str] = &[st("\"", "\"", b'\\', false), st("'", "'", b'\\', false)];
const CS_STRINGS: &[Str] = &[
    st("\"\"\"", "\"\"\"", 0, true),
    st("$@\"", "\"", 0, true),
    st("@$\"", "\"", 0, true),
    st("@\"", "\"", 0, true),
    st("$\"", "\"", b'\\', false),
    st("\"", "\"", b'\\', false),
    st("'", "'", b'\\', false),
];
const RUST_STRINGS: &[Str] = &[st("\"", "\"", b'\\', true)];
const GO_STRINGS: &[Str] = &[st("\"", "\"", b'\\', false), st("`", "`", 0, true), st("'", "'", b'\\', false)];
const JAVA_STRINGS: &[Str] =
    &[st("\"\"\"", "\"\"\"", b'\\', true), st("\"", "\"", b'\\', false), st("'", "'", b'\\', false)];
const PY_STRINGS: &[Str] = &[
    st("\"\"\"", "\"\"\"", b'\\', true),
    st("'''", "'''", b'\\', true),
    st("\"", "\"", b'\\', false),
    st("'", "'", b'\\', false),
];
const JS_STRINGS: &[Str] = &[st("\"", "\"", b'\\', false), st("'", "'", b'\\', false), st("`", "`", b'\\', true)];
const JSON_STRINGS: &[Str] = &[st("\"", "\"", b'\\', false)];
const PLAIN_QUOTES: &[Str] = &[st("\"", "\"", 0, false), st("'", "'", 0, false)];
const TOML_STRINGS: &[Str] = &[
    st("\"\"\"", "\"\"\"", b'\\', true),
    st("'''", "'''", 0, true),
    st("\"", "\"", b'\\', false),
    st("'", "'", 0, false),
];
const YAML_STRINGS: &[Str] = &[st("\"", "\"", b'\\', false), st("'", "'", 0, false)];
const SHELL_STRINGS: &[Str] = &[
    Str { interp: true, ..st("\"", "\"", b'\\', true) },
    st("'", "'", 0, true),
    Str { interp: true, ..st("`", "`", b'\\', true) },
];
const PS_STRINGS: &[Str] = &[
    Str { interp: true, line_start_close: true, ..st("@\"", "\"@", b'`', true) },
    Str { line_start_close: true, ..st("@'", "'@", 0, true) },
    Str { interp: true, ..st("\"", "\"", b'`', true) },
    st("'", "'", 0, true),
];
const BATCH_STRINGS: &[Str] = &[st("\"", "\"", 0, false)];
const SQL_STRINGS: &[Str] = &[st("'", "'", 0, true), st("\"", "\"", 0, false)];
const R_STRINGS: &[Str] = &[st("\"", "\"", b'\\', true), st("'", "'", b'\\', true)];
const LUA_STRINGS: &[Str] = &[st("[[", "]]", 0, true), st("\"", "\"", b'\\', false), st("'", "'", b'\\', false)];
const MAKE_STRINGS: &[Str] = &[st("\"", "\"", b'\\', false), st("'", "'", 0, false)];
const CMAKE_STRINGS: &[Str] = &[st("[[", "]]", 0, true), st("\"", "\"", b'\\', true)];

const SLASH_LINE: &[&str] = &["//"];
const SLASH_BLOCK: &[(&str, &str)] = &[("/*", "*/")];
const HASH: &[&str] = &["#"];

// ---- keyword tables -----------------------------------------------------------------------------------------------

const C_KEYWORDS: &[&str] = &[
    "_Alignas",
    "_Alignof",
    "_Atomic",
    "_Generic",
    "_Noreturn",
    "_Static_assert",
    "_Thread_local",
    "auto",
    "break",
    "case",
    "const",
    "continue",
    "default",
    "do",
    "else",
    "enum",
    "extern",
    "for",
    "goto",
    "if",
    "inline",
    "register",
    "restrict",
    "return",
    "sizeof",
    "static",
    "struct",
    "switch",
    "typedef",
    "union",
    "volatile",
    "while",
];
const C_TYPES: &[&str] = &[
    "FILE",
    "_Bool",
    "_Complex",
    "bool",
    "char",
    "double",
    "float",
    "int",
    "int16_t",
    "int32_t",
    "int64_t",
    "int8_t",
    "intptr_t",
    "long",
    "ptrdiff_t",
    "short",
    "signed",
    "size_t",
    "ssize_t",
    "uint16_t",
    "uint32_t",
    "uint64_t",
    "uint8_t",
    "uintptr_t",
    "unsigned",
    "void",
    "wchar_t",
];
const C_CONSTANTS: &[&str] = &["EOF", "NULL", "false", "stderr", "stdin", "stdout", "true"];
const CPP_KEYWORDS: &[&str] = &[
    "alignas",
    "alignof",
    "and",
    "and_eq",
    "asm",
    "auto",
    "bitand",
    "bitor",
    "break",
    "case",
    "catch",
    "class",
    "co_await",
    "co_return",
    "co_yield",
    "compl",
    "concept",
    "const",
    "const_cast",
    "consteval",
    "constexpr",
    "constinit",
    "continue",
    "decltype",
    "default",
    "delete",
    "do",
    "dynamic_cast",
    "else",
    "enum",
    "explicit",
    "export",
    "extern",
    "final",
    "for",
    "friend",
    "goto",
    "if",
    "inline",
    "mutable",
    "namespace",
    "new",
    "noexcept",
    "not",
    "not_eq",
    "operator",
    "or",
    "or_eq",
    "override",
    "private",
    "protected",
    "public",
    "register",
    "reinterpret_cast",
    "requires",
    "return",
    "sizeof",
    "static",
    "static_assert",
    "static_cast",
    "struct",
    "switch",
    "template",
    "this",
    "thread_local",
    "throw",
    "try",
    "typedef",
    "typeid",
    "typename",
    "union",
    "using",
    "virtual",
    "volatile",
    "while",
    "xor",
    "xor_eq",
];
const CPP_TYPES: &[&str] = &[
    "bool",
    "char",
    "char16_t",
    "char32_t",
    "char8_t",
    "double",
    "float",
    "int",
    "int16_t",
    "int32_t",
    "int64_t",
    "int8_t",
    "intptr_t",
    "long",
    "map",
    "ptrdiff_t",
    "set",
    "shared_ptr",
    "short",
    "signed",
    "size_t",
    "ssize_t",
    "string",
    "uint16_t",
    "uint32_t",
    "uint64_t",
    "uint8_t",
    "uintptr_t",
    "unique_ptr",
    "unsigned",
    "vector",
    "void",
    "wchar_t",
];
const CPP_CONSTANTS: &[&str] = &["NULL", "false", "nullptr", "true"];
const CS_KEYWORDS: &[&str] = &[
    "abstract",
    "as",
    "async",
    "await",
    "base",
    "break",
    "case",
    "catch",
    "checked",
    "class",
    "const",
    "continue",
    "default",
    "delegate",
    "do",
    "else",
    "enum",
    "event",
    "explicit",
    "extern",
    "finally",
    "fixed",
    "for",
    "foreach",
    "get",
    "global",
    "goto",
    "if",
    "implicit",
    "in",
    "init",
    "interface",
    "internal",
    "is",
    "lock",
    "nameof",
    "namespace",
    "new",
    "operator",
    "out",
    "override",
    "params",
    "partial",
    "private",
    "protected",
    "public",
    "readonly",
    "record",
    "ref",
    "return",
    "sealed",
    "set",
    "sizeof",
    "stackalloc",
    "static",
    "struct",
    "switch",
    "this",
    "throw",
    "try",
    "typeof",
    "unchecked",
    "unsafe",
    "using",
    "var",
    "virtual",
    "volatile",
    "when",
    "where",
    "while",
    "with",
    "yield",
];
const CS_TYPES: &[&str] = &[
    "bool", "byte", "char", "decimal", "double", "dynamic", "float", "int", "long", "nint", "nuint", "object", "sbyte",
    "short", "string", "uint", "ulong", "ushort", "void",
];
const NULL_BOOL: &[&str] = &["false", "null", "true"];
const RUST_KEYWORDS: &[&str] = &[
    "Self", "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern", "fn",
    "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return", "self", "static",
    "struct", "super", "trait", "type", "union", "unsafe", "use", "where", "while", "yield",
];
const RUST_TYPES: &[&str] = &[
    "bool", "char", "f32", "f64", "i128", "i16", "i32", "i64", "i8", "isize", "str", "u128", "u16", "u32", "u64", "u8",
    "usize",
];
const RUST_CONSTANTS: &[&str] = &["Err", "None", "Ok", "Some", "false", "true"];
const GO_KEYWORDS: &[&str] = &[
    "break",
    "case",
    "chan",
    "const",
    "continue",
    "default",
    "defer",
    "else",
    "fallthrough",
    "for",
    "func",
    "go",
    "goto",
    "if",
    "import",
    "interface",
    "map",
    "package",
    "range",
    "return",
    "select",
    "struct",
    "switch",
    "type",
    "var",
];
const GO_TYPES: &[&str] = &[
    "any",
    "bool",
    "byte",
    "complex128",
    "complex64",
    "error",
    "float32",
    "float64",
    "int",
    "int16",
    "int32",
    "int64",
    "int8",
    "rune",
    "string",
    "uint",
    "uint16",
    "uint32",
    "uint64",
    "uint8",
    "uintptr",
];
const GO_CONSTANTS: &[&str] = &["false", "iota", "nil", "true"];
const JAVA_KEYWORDS: &[&str] = &[
    "abstract",
    "assert",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "default",
    "do",
    "else",
    "enum",
    "extends",
    "final",
    "finally",
    "for",
    "goto",
    "if",
    "implements",
    "import",
    "instanceof",
    "interface",
    "native",
    "new",
    "package",
    "permits",
    "private",
    "protected",
    "public",
    "record",
    "return",
    "sealed",
    "static",
    "strictfp",
    "super",
    "switch",
    "synchronized",
    "this",
    "throw",
    "throws",
    "transient",
    "try",
    "var",
    "volatile",
    "while",
    "yield",
];
const JAVA_TYPES: &[&str] = &["boolean", "byte", "char", "double", "float", "int", "long", "short", "void"];
const PY_KEYWORDS: &[&str] = &[
    "and", "as", "assert", "async", "await", "break", "case", "class", "continue", "def", "del", "elif", "else",
    "except", "finally", "for", "from", "global", "if", "import", "in", "is", "lambda", "match", "nonlocal", "not",
    "or", "pass", "raise", "return", "try", "while", "with", "yield",
];
const PY_TYPES: &[&str] = &[
    "bool",
    "bytearray",
    "bytes",
    "complex",
    "dict",
    "float",
    "frozenset",
    "int",
    "list",
    "object",
    "set",
    "str",
    "tuple",
    "type",
];
const PY_CONSTANTS: &[&str] = &["Ellipsis", "False", "None", "NotImplemented", "True", "cls", "self"];
const JS_KEYWORDS: &[&str] = &[
    "async",
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "else",
    "export",
    "extends",
    "finally",
    "for",
    "from",
    "function",
    "get",
    "if",
    "import",
    "in",
    "instanceof",
    "let",
    "new",
    "of",
    "return",
    "set",
    "static",
    "super",
    "switch",
    "this",
    "throw",
    "try",
    "typeof",
    "var",
    "void",
    "while",
    "with",
    "yield",
];
const JS_TYPES: &[&str] = &[
    "Array", "Boolean", "Date", "Error", "JSON", "Map", "Math", "Number", "Object", "Promise", "RegExp", "Set",
    "String", "Symbol",
];
const JS_CONSTANTS: &[&str] = &["Infinity", "NaN", "false", "null", "true", "undefined"];
const TS_KEYWORDS: &[&str] = &[
    "abstract",
    "as",
    "asserts",
    "async",
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "declare",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "export",
    "extends",
    "finally",
    "for",
    "from",
    "function",
    "get",
    "if",
    "implements",
    "import",
    "in",
    "infer",
    "instanceof",
    "interface",
    "is",
    "keyof",
    "let",
    "module",
    "namespace",
    "new",
    "of",
    "override",
    "private",
    "protected",
    "public",
    "readonly",
    "return",
    "satisfies",
    "set",
    "static",
    "super",
    "switch",
    "this",
    "throw",
    "try",
    "type",
    "typeof",
    "unique",
    "var",
    "void",
    "while",
    "with",
    "yield",
];
const TS_TYPES: &[&str] = &[
    "Array", "Date", "Error", "Map", "Promise", "Record", "Set", "any", "bigint", "boolean", "never", "number",
    "object", "string", "symbol", "unknown",
];
const CSS_KEYWORDS: &[&str] = &["and", "from", "important", "not", "only", "to"];
const CSS_CONSTANTS: &[&str] = &["auto", "currentColor", "inherit", "initial", "none", "transparent", "unset"];
const INI_CONSTANTS: &[&str] = &["false", "no", "off", "on", "true", "yes"];
const TOML_CONSTANTS: &[&str] = &["false", "inf", "nan", "true"];
const YAML_CONSTANTS: &[&str] = &["False", "Null", "True", "false", "no", "null", "off", "on", "true", "yes"];
const SHELL_KEYWORDS: &[&str] = &[
    "alias", "break", "case", "continue", "declare", "do", "done", "elif", "else", "esac", "eval", "exec", "exit",
    "export", "fi", "for", "function", "if", "in", "local", "readonly", "return", "select", "set", "shift", "source",
    "then", "time", "trap", "typeset", "unset", "until", "while",
];
const PS_KEYWORDS: &[&str] = &[
    "begin",
    "break",
    "catch",
    "class",
    "continue",
    "data",
    "define",
    "do",
    "dynamicparam",
    "else",
    "elseif",
    "end",
    "enum",
    "exit",
    "filter",
    "finally",
    "for",
    "foreach",
    "from",
    "function",
    "hidden",
    "if",
    "in",
    "param",
    "process",
    "return",
    "static",
    "switch",
    "throw",
    "trap",
    "try",
    "until",
    "using",
    "var",
    "while",
    "workflow",
];
const BATCH_KEYWORDS: &[&str] = &[
    "call",
    "cd",
    "cls",
    "defined",
    "do",
    "echo",
    "else",
    "endlocal",
    "equ",
    "errorlevel",
    "exist",
    "exit",
    "for",
    "geq",
    "goto",
    "gtr",
    "if",
    "in",
    "leq",
    "lss",
    "neq",
    "not",
    "off",
    "on",
    "pause",
    "popd",
    "pushd",
    "set",
    "setlocal",
    "shift",
    "start",
    "title",
];
const SQL_KEYWORDS: &[&str] = &[
    "add",
    "all",
    "alter",
    "and",
    "as",
    "asc",
    "begin",
    "between",
    "by",
    "cascade",
    "case",
    "check",
    "column",
    "commit",
    "constraint",
    "create",
    "cross",
    "database",
    "declare",
    "default",
    "delete",
    "desc",
    "distinct",
    "drop",
    "else",
    "end",
    "exists",
    "foreign",
    "from",
    "full",
    "function",
    "grant",
    "group",
    "having",
    "if",
    "in",
    "index",
    "inner",
    "insert",
    "into",
    "is",
    "join",
    "key",
    "left",
    "like",
    "limit",
    "not",
    "offset",
    "on",
    "or",
    "order",
    "outer",
    "primary",
    "procedure",
    "recursive",
    "references",
    "return",
    "returns",
    "revoke",
    "right",
    "rollback",
    "schema",
    "select",
    "set",
    "table",
    "then",
    "top",
    "transaction",
    "trigger",
    "union",
    "unique",
    "update",
    "values",
    "view",
    "when",
    "where",
    "while",
    "with",
];
const SQL_TYPES: &[&str] = &[
    "bigint",
    "blob",
    "bool",
    "boolean",
    "char",
    "clob",
    "date",
    "datetime",
    "decimal",
    "double",
    "float",
    "int",
    "integer",
    "numeric",
    "nvarchar",
    "real",
    "serial",
    "smallint",
    "text",
    "time",
    "timestamp",
    "tinyint",
    "uuid",
    "varchar",
];
const R_KEYWORDS: &[&str] =
    &["break", "else", "for", "function", "if", "in", "library", "next", "repeat", "require", "return", "while"];
const R_CONSTANTS: &[&str] =
    &["F", "FALSE", "Inf", "NA", "NA_character_", "NA_integer_", "NA_real_", "NULL", "NaN", "T", "TRUE"];
const LUA_KEYWORDS: &[&str] = &[
    "and", "break", "do", "else", "elseif", "end", "for", "function", "goto", "if", "in", "local", "not", "or",
    "repeat", "return", "then", "until", "while",
];
const LUA_CONSTANTS: &[&str] = &["false", "nil", "true"];
const MAKE_KEYWORDS: &[&str] = &[
    "define", "else", "endef", "endif", "export", "ifdef", "ifeq", "ifndef", "ifneq", "include", "override", "private",
    "sinclude", "unexport", "vpath",
];
const CMAKE_KEYWORDS: &[&str] = &[
    "and",
    "block",
    "break",
    "continue",
    "defined",
    "else",
    "elseif",
    "endblock",
    "endforeach",
    "endfunction",
    "endif",
    "endmacro",
    "endwhile",
    "equal",
    "exists",
    "foreach",
    "function",
    "greater",
    "if",
    "in_list",
    "less",
    "macro",
    "matches",
    "not",
    "or",
    "return",
    "strequal",
    "version_equal",
    "version_greater",
    "version_less",
    "while",
];
const CMAKE_CONSTANTS: &[&str] = &["false", "no", "off", "on", "true", "yes"];

const fn less(a: &[u8], b: &[u8]) -> bool {
    let mut i = 0;
    while i < a.len() && i < b.len() && a[i] == b[i] {
        i += 1;
    }
    if i < a.len() && i < b.len() {
        a[i] < b[i]
    } else {
        a.len() < b.len()
    }
}

/// Word lists are binary searched, so they must be strictly sorted by bytes (lowercase for case-insensitive specs).
pub(crate) const fn sorted(list: &[&str]) -> bool {
    let mut ok = true;
    let mut i = 1;
    while i < list.len() {
        ok = ok && less(list[i - 1].as_bytes(), list[i].as_bytes());
        i += 1;
    }
    ok
}

const _: () = assert!(
    sorted(C_KEYWORDS)
        && sorted(C_TYPES)
        && sorted(C_CONSTANTS)
        && sorted(CPP_KEYWORDS)
        && sorted(CPP_TYPES)
        && sorted(CPP_CONSTANTS)
        && sorted(CS_KEYWORDS)
        && sorted(CS_TYPES)
        && sorted(NULL_BOOL)
        && sorted(RUST_KEYWORDS)
        && sorted(RUST_TYPES)
        && sorted(RUST_CONSTANTS)
        && sorted(GO_KEYWORDS)
        && sorted(GO_TYPES)
        && sorted(GO_CONSTANTS)
        && sorted(JAVA_KEYWORDS)
        && sorted(JAVA_TYPES)
        && sorted(PY_KEYWORDS)
        && sorted(PY_TYPES)
        && sorted(PY_CONSTANTS)
        && sorted(JS_KEYWORDS)
        && sorted(JS_TYPES)
        && sorted(JS_CONSTANTS)
        && sorted(TS_KEYWORDS)
        && sorted(TS_TYPES)
        && sorted(CSS_KEYWORDS)
        && sorted(CSS_CONSTANTS)
        && sorted(INI_CONSTANTS)
        && sorted(TOML_CONSTANTS)
        && sorted(YAML_CONSTANTS)
        && sorted(SHELL_KEYWORDS)
        && sorted(PS_KEYWORDS)
        && sorted(BATCH_KEYWORDS)
        && sorted(SQL_KEYWORDS)
        && sorted(SQL_TYPES)
        && sorted(R_KEYWORDS)
        && sorted(R_CONSTANTS)
        && sorted(LUA_KEYWORDS)
        && sorted(MAKE_KEYWORDS)
        && sorted(CMAKE_KEYWORDS)
        && sorted(CMAKE_CONSTANTS)
        && sorted(LUA_CONSTANTS)
);

// ---- code specs ---------------------------------------------------------------------------------------------------

static C: CodeSpec = CodeSpec {
    line_comments: SLASH_LINE,
    block_comments: SLASH_BLOCK,
    strings: C_STRINGS,
    keywords: C_KEYWORDS,
    types: C_TYPES,
    constants: C_CONSTANTS,
    preprocessor: true,
    flavor: Flavor::C,
    ..BASE
};
static CPP: CodeSpec =
    CodeSpec { keywords: CPP_KEYWORDS, types: CPP_TYPES, constants: CPP_CONSTANTS, flavor: Flavor::Cpp, ..C };
static CSHARP: CodeSpec = CodeSpec {
    strings: CS_STRINGS,
    keywords: CS_KEYWORDS,
    types: CS_TYPES,
    constants: NULL_BOOL,
    upper_types: true,
    flavor: Flavor::Generic,
    ..C
};
static RUST: CodeSpec = CodeSpec {
    line_comments: SLASH_LINE,
    block_comments: SLASH_BLOCK,
    nested_comments: true,
    strings: RUST_STRINGS,
    keywords: RUST_KEYWORDS,
    types: RUST_TYPES,
    constants: RUST_CONSTANTS,
    upper_types: true,
    flavor: Flavor::Rust,
    ..BASE
};
static GO: CodeSpec = CodeSpec {
    line_comments: SLASH_LINE,
    block_comments: SLASH_BLOCK,
    strings: GO_STRINGS,
    keywords: GO_KEYWORDS,
    types: GO_TYPES,
    constants: GO_CONSTANTS,
    ..BASE
};
static JAVA: CodeSpec = CodeSpec {
    line_comments: SLASH_LINE,
    block_comments: SLASH_BLOCK,
    strings: JAVA_STRINGS,
    keywords: JAVA_KEYWORDS,
    types: JAVA_TYPES,
    constants: NULL_BOOL,
    upper_types: true,
    annotations: true,
    ..BASE
};
static PYTHON: CodeSpec = CodeSpec {
    line_comments: HASH,
    strings: PY_STRINGS,
    keywords: PY_KEYWORDS,
    types: PY_TYPES,
    constants: PY_CONSTANTS,
    annotations: true,
    flavor: Flavor::Python,
    ..BASE
};
static JAVASCRIPT: CodeSpec = CodeSpec {
    line_comments: SLASH_LINE,
    block_comments: SLASH_BLOCK,
    strings: JS_STRINGS,
    keywords: JS_KEYWORDS,
    types: JS_TYPES,
    constants: JS_CONSTANTS,
    ident_extra: b"$",
    ..BASE
};
static TYPESCRIPT: CodeSpec =
    CodeSpec { keywords: TS_KEYWORDS, types: TS_TYPES, upper_types: true, annotations: true, ..JAVASCRIPT };
static JSON: CodeSpec = CodeSpec {
    line_comments: SLASH_LINE,
    block_comments: SLASH_BLOCK,
    strings: JSON_STRINGS,
    constants: NULL_BOOL,
    flavor: Flavor::Json,
    ..BASE
};
static CSS: CodeSpec = CodeSpec {
    block_comments: SLASH_BLOCK,
    strings: C_STRINGS,
    keywords: CSS_KEYWORDS,
    constants: CSS_CONSTANTS,
    ident_dash: true,
    flavor: Flavor::Css,
    ..BASE
};
static INI: CodeSpec = CodeSpec {
    line_comments: &[";", "#"],
    strings: PLAIN_QUOTES,
    constants: INI_CONSTANTS,
    ignore_case: true,
    comment_needs_space: true,
    quote_needs_boundary: true,
    flavor: Flavor::Ini,
    ..BASE
};
static TOML: CodeSpec =
    CodeSpec { line_comments: HASH, strings: TOML_STRINGS, constants: TOML_CONSTANTS, flavor: Flavor::Toml, ..BASE };
static YAML: CodeSpec = CodeSpec {
    line_comments: HASH,
    strings: YAML_STRINGS,
    constants: YAML_CONSTANTS,
    comment_needs_space: true,
    quote_needs_boundary: true,
    flavor: Flavor::Yaml,
    ..BASE
};
static SHELL: CodeSpec = CodeSpec {
    line_comments: HASH,
    strings: SHELL_STRINGS,
    keywords: SHELL_KEYWORDS,
    comment_needs_space: true,
    vars: Vars::Shell,
    ..BASE
};
static POWERSHELL: CodeSpec = CodeSpec {
    line_comments: HASH,
    block_comments: &[("<#", "#>")],
    strings: PS_STRINGS,
    keywords: PS_KEYWORDS,
    ignore_case: true,
    ident_dash: true,
    vars: Vars::PowerShell,
    flavor: Flavor::PowerShell,
    ..BASE
};
static BATCH: CodeSpec = CodeSpec {
    strings: BATCH_STRINGS,
    keywords: BATCH_KEYWORDS,
    ignore_case: true,
    vars: Vars::Batch,
    flavor: Flavor::Batch,
    ..BASE
};
static SQL: CodeSpec = CodeSpec {
    line_comments: &["--"],
    block_comments: SLASH_BLOCK,
    strings: SQL_STRINGS,
    keywords: SQL_KEYWORDS,
    types: SQL_TYPES,
    constants: NULL_BOOL,
    ignore_case: true,
    vars: Vars::Sql,
    ..BASE
};
static R: CodeSpec =
    CodeSpec { line_comments: HASH, strings: R_STRINGS, keywords: R_KEYWORDS, constants: R_CONSTANTS, ..BASE };
static LUA: CodeSpec = CodeSpec {
    line_comments: &["--"],
    block_comments: &[("--[[", "]]")],
    strings: LUA_STRINGS,
    keywords: LUA_KEYWORDS,
    constants: LUA_CONSTANTS,
    ..BASE
};
static MAKEFILE: CodeSpec = CodeSpec {
    line_comments: HASH,
    strings: MAKE_STRINGS,
    keywords: MAKE_KEYWORDS,
    vars: Vars::Make,
    flavor: Flavor::Make,
    ..BASE
};
static CMAKE: CodeSpec = CodeSpec {
    line_comments: HASH,
    block_comments: &[("#[[", "]]")],
    strings: CMAKE_STRINGS,
    keywords: CMAKE_KEYWORDS,
    constants: CMAKE_CONSTANTS,
    ignore_case: true,
    vars: Vars::CMake,
    ..BASE
};

// ---- language table -----------------------------------------------------------------------------------------------

const fn lang(id: &'static str, name: &'static str, extensions: &'static [&'static str], mode: Mode) -> Language {
    Language {
        id,
        name,
        extensions,
        file_names: &[],
        fence_tags: &[],
        line_comment: None,
        block_comment: None,
        mode,
        fold: FoldStyle::None,
    }
}

const SLASH_COMMENTS: Language = Language {
    line_comment: Some("//"),
    block_comment: Some(("/*", "*/")),
    fold: FoldStyle::Braces,
    ..lang("", "", &[], Mode::Plain)
};
const HASH_COMMENT: Option<&str> = Some("#");
const XML_COMMENT: Option<(&str, &str)> = Some(("<!--", "-->"));

static LANGUAGES: [Language; 29] = [
    Language {
        fence_tags: &["text", "plain", "plaintext"],
        ..lang("plain", "Plain text", &["txt", "text", "log"], Mode::Plain)
    },
    Language { id: "c", name: "C", extensions: &["c"], mode: Mode::Code(&C), ..SLASH_COMMENTS },
    Language {
        id: "cpp",
        name: "C++",
        extensions: &["cpp", "cc", "cxx", "c++", "hpp", "hh", "hxx", "h", "inl", "ino"],
        fence_tags: &["c++"],
        mode: Mode::Code(&CPP),
        ..SLASH_COMMENTS
    },
    Language {
        id: "csharp",
        name: "C#",
        extensions: &["cs", "csx"],
        fence_tags: &["c#"],
        mode: Mode::Code(&CSHARP),
        ..SLASH_COMMENTS
    },
    Language { id: "rust", name: "Rust", extensions: &["rs"], mode: Mode::Code(&RUST), ..SLASH_COMMENTS },
    Language {
        id: "go",
        name: "Go",
        extensions: &["go"],
        fence_tags: &["golang"],
        mode: Mode::Code(&GO),
        ..SLASH_COMMENTS
    },
    Language { id: "java", name: "Java", extensions: &["java"], mode: Mode::Code(&JAVA), ..SLASH_COMMENTS },
    Language {
        line_comment: HASH_COMMENT,
        file_names: &["sconstruct", "sconscript"],
        fence_tags: &["python3", "py3"],
        fold: FoldStyle::Indent,
        ..lang("python", "Python", &["py", "pyw", "pyi"], Mode::Code(&PYTHON))
    },
    Language {
        id: "javascript",
        name: "JavaScript",
        extensions: &["js", "mjs", "cjs", "jsx"],
        fence_tags: &["node"],
        mode: Mode::Code(&JAVASCRIPT),
        ..SLASH_COMMENTS
    },
    Language {
        id: "typescript",
        name: "TypeScript",
        extensions: &["ts", "mts", "cts", "tsx"],
        mode: Mode::Code(&TYPESCRIPT),
        ..SLASH_COMMENTS
    },
    Language {
        fold: FoldStyle::Brackets,
        file_names: &[".prettierrc", ".babelrc", ".eslintrc"],
        ..lang(
            "json",
            "JSON",
            &["json", "jsonc", "json5", "geojson", "ipynb", "kladde", "code-workspace"],
            Mode::Code(&JSON),
        )
    },
    Language {
        block_comment: XML_COMMENT,
        fold: FoldStyle::Tags { html: false },
        ..lang(
            "xml",
            "XML",
            &[
                "xml", "xsd", "xsl", "xslt", "svg", "csproj", "vcxproj", "props", "targets", "config", "plist", "resx",
                "xaml", "nuspec", "wsdl", "rss", "atom",
            ],
            Mode::Xml,
        )
    },
    Language {
        block_comment: XML_COMMENT,
        fold: FoldStyle::Tags { html: true },
        ..lang("html", "HTML", &["html", "htm", "xhtml"], Mode::Xml)
    },
    Language {
        block_comment: Some(("/*", "*/")),
        fold: FoldStyle::Braces,
        ..lang("css", "CSS", &["css", "scss", "less"], Mode::Code(&CSS))
    },
    Language {
        block_comment: XML_COMMENT,
        fence_tags: &["quarto", "rmarkdown"],
        fold: FoldStyle::Headings,
        ..lang("markdown", "Markdown / Quarto", &["md", "markdown", "mdown", "mkd", "qmd", "rmd"], Mode::Markdown)
    },
    Language {
        line_comment: Some(";"),
        file_names: &[".editorconfig", ".gitconfig", ".npmrc", "desktop.ini"],
        fence_tags: &["cfg", "dosini"],
        fold: FoldStyle::Sections,
        ..lang(
            "ini",
            "INI",
            &["ini", "cfg", "conf", "inf", "reg", "properties", "desktop", "gitconfig"],
            Mode::Code(&INI),
        )
    },
    Language {
        line_comment: HASH_COMMENT,
        file_names: &["cargo.lock", "pipfile", "poetry.lock"],
        fold: FoldStyle::Sections,
        ..lang("toml", "TOML", &["toml"], Mode::Code(&TOML))
    },
    Language {
        line_comment: HASH_COMMENT,
        file_names: &[".clang-format", ".clang-tidy"],
        fold: FoldStyle::Indent,
        ..lang("yaml", "YAML", &["yaml", "yml"], Mode::Code(&YAML))
    },
    Language {
        line_comment: HASH_COMMENT,
        file_names: &[
            ".bashrc",
            ".bash_profile",
            ".bash_login",
            ".bash_logout",
            ".bash_aliases",
            ".profile",
            ".zshrc",
            ".zprofile",
            ".zshenv",
            ".zlogin",
            ".kshrc",
            "pkgbuild",
        ],
        fence_tags: &["sh", "bash", "zsh", "shell", "shellscript", "console", "ksh"],
        fold: FoldStyle::Braces,
        ..lang("shell", "Shell", &["sh", "bash", "zsh", "ksh", "command"], Mode::Code(&SHELL))
    },
    Language {
        line_comment: HASH_COMMENT,
        block_comment: Some(("<#", "#>")),
        fence_tags: &["ps", "pwsh", "posh"],
        fold: FoldStyle::Braces,
        ..lang("powershell", "PowerShell", &["ps1", "psm1", "psd1"], Mode::Code(&POWERSHELL))
    },
    Language {
        line_comment: Some("REM "),
        fence_tags: &["bat", "cmd", "dos"],
        ..lang("batch", "Batch", &["bat", "cmd"], Mode::Code(&BATCH))
    },
    Language {
        line_comment: Some("--"),
        block_comment: Some(("/*", "*/")),
        fence_tags: &["mysql", "postgresql", "psql", "sqlite", "tsql", "plsql"],
        ..lang("sql", "SQL", &["sql", "ddl", "dml"], Mode::Code(&SQL))
    },
    Language {
        line_comment: HASH_COMMENT,
        file_names: &[".rprofile"],
        fold: FoldStyle::Braces,
        ..lang("r", "R", &["r"], Mode::Code(&R))
    },
    Language {
        line_comment: Some("--"),
        block_comment: Some(("--[[", "]]")),
        fold: FoldStyle::Indent,
        ..lang("lua", "Lua", &["lua"], Mode::Code(&LUA))
    },
    Language {
        line_comment: HASH_COMMENT,
        file_names: &["makefile", "gnumakefile", "bsdmakefile"],
        fence_tags: &["make", "mk"],
        ..lang("makefile", "Makefile", &["mk", "mak", "make"], Mode::Code(&MAKEFILE))
    },
    Language {
        line_comment: HASH_COMMENT,
        block_comment: Some(("#[[", "]]")),
        file_names: &["cmakelists.txt"],
        fold: FoldStyle::Indent,
        ..lang("cmake", "CMake", &["cmake"], Mode::Code(&CMAKE))
    },
    Language { fence_tags: &["patch", "udiff"], ..lang("diff", "Diff", &["diff", "patch", "rej"], Mode::Diff) },
    lang("csv", "CSV", &["csv"], Mode::Csv(b',')),
    lang("tsv", "TSV", &["tsv", "tab"], Mode::Csv(b'\t')),
];
