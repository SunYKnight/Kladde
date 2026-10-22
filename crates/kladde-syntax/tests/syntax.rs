use std::path::Path;

use kladde_syntax::{
    compute_folds, highlight_line, language_by_id, language_for_fence, language_for_path, languages, FoldRange,
    HighlightCache, Language, LineState, Token, TokenKind as K, RAINBOW_COLUMNS,
};

fn lang(id: &str) -> &'static Language {
    language_by_id(id).expect("language exists")
}

/// Highlights `text` line by line; returns (text, kind) pairs per line.
fn lex(id: &str, text: &str) -> Vec<Vec<(String, K)>> {
    let l = lang(id);
    let mut state = LineState::default();
    text.lines()
        .map(|line| {
            let (tokens, next) = highlight_line(l, state, line);
            state = next;
            tokens.iter().map(|t| (line[t.range.clone()].to_string(), t.kind)).collect()
        })
        .collect()
}

fn has(line: &[(String, K)], text: &str, kind: K) -> bool {
    line.iter().any(|(t, k)| t == text && *k == kind)
}

fn kind_of(line: &[(String, K)], text: &str) -> Option<K> {
    line.iter().find(|(t, _)| t.contains(text)).map(|(_, k)| *k)
}

#[test]
fn c_block_comment_spans_lines() {
    let lines = lex("c", "int a; /* start\nstill { comment\nend */ int b;\nint c;");
    assert!(has(&lines[0], "/* start", K::Comment));
    assert!(has(&lines[1], "still { comment", K::Comment));
    assert!(has(&lines[2], "end */", K::Comment));
    assert!(has(&lines[2], "int", K::Type));
    assert!(has(&lines[3], "int", K::Type));
}

#[test]
fn rust_nested_comments() {
    let lines = lex("rust", "/* a /* b */ still\n*/ fn x() {}");
    assert!(has(&lines[0], "/* a /* b */ still", K::Comment));
    assert!(has(&lines[1], "*/", K::Comment));
    assert!(has(&lines[1], "fn", K::Keyword));
    assert!(has(&lines[1], "x", K::Function));
}

#[test]
fn rust_raw_strings_chars_and_lifetimes() {
    let lines = lex("rust", "let s = r#\"a \"quoted\" b\"#; let c = '\\n'; fn f<'a>() {}");
    assert!(has(&lines[0], "r#\"a \"quoted\" b\"#", K::String));
    assert!(has(&lines[0], "\\n", K::Escape));
    assert!(has(&lines[0], "'a", K::Type));
}

#[test]
fn python_triple_quoted_string_across_lines() {
    let lines = lex("python", "x = \"\"\"first\nif not code\n\"\"\" + y\nif z:");
    assert!(has(&lines[0], "\"\"\"first", K::String));
    assert!(has(&lines[1], "if not code", K::String));
    assert!(has(&lines[2], "\"\"\"", K::String));
    assert!(!has(&lines[2], "y", K::String));
    assert!(has(&lines[3], "if", K::Keyword));
}

#[test]
fn cpp_raw_string_across_lines() {
    let lines = lex("cpp", "auto s = R\"x(one )\" two\nthree)x\"; int y;");
    assert_eq!(kind_of(&lines[0], "one"), Some(K::String));
    assert!(has(&lines[1], "three)x\"", K::String));
    assert!(has(&lines[1], "int", K::Type));
}

#[test]
fn powershell_here_string() {
    let text = "$s = @\"\nHello $name \"quoted\"\n  \"@ not closed\n\"@\nGet-Item -Path $s";
    let lines = lex("powershell", text);
    assert!(has(&lines[1], "$name", K::Variable));
    assert!(has(&lines[1], " \"quoted\"", K::String));
    assert!(has(&lines[2], "  \"@ not closed", K::String));
    assert!(has(&lines[3], "\"@", K::String));
    assert!(has(&lines[4], "Get-Item", K::Function));
    assert!(has(&lines[4], "-Path", K::Attribute));
    assert!(has(&lines[4], "$s", K::Variable));
}

#[test]
fn shell_and_batch_variables() {
    let sh = lex("shell", "echo \"$HOME/${dir}\" # note");
    assert!(has(&sh[0], "$HOME", K::Variable));
    assert!(has(&sh[0], "${dir}", K::Variable));
    assert!(has(&sh[0], "# note", K::Comment));
    let bat = lex("batch", "@echo off\nset X=%PATH%;%~dp0\nREM comment\n:label");
    assert!(has(&bat[1], "%PATH%", K::Variable));
    assert!(has(&bat[1], "%~dp0", K::Variable));
    assert!(has(&bat[2], "REM comment", K::Comment));
    assert!(has(&bat[3], ":label", K::Function));
}

#[test]
fn markdown_fence_switches_language_and_back() {
    let text = "# Title\n```{python}\ndef f():  # *not emphasis*\n    return 1\n```\nSome *emphasis* and `code`";
    let lines = lex("markdown", text);
    assert!(has(&lines[0], "# Title", K::Heading));
    assert!(has(&lines[2], "def", K::Keyword));
    assert!(has(&lines[2], "# *not emphasis*", K::Comment));
    assert!(has(&lines[3], "return", K::Keyword));
    assert!(has(&lines[4], "```", K::Code));
    assert!(has(&lines[5], "*emphasis*", K::Emphasis));
    assert!(has(&lines[5], "`code`", K::Code));
    assert!(!lines[5].iter().any(|(_, k)| *k == K::Keyword));
}

#[test]
fn markdown_fence_carries_inner_state_and_front_matter() {
    let text = "---\ntitle: \"x\"\n---\n~~~~ rust\n/* open\nfn */ x\n~~~\n~~~~\nplain [link](http://a.b)";
    let lines = lex("markdown", text);
    assert!(has(&lines[1], "title", K::Attribute));
    assert!(has(&lines[2], "---", K::Preprocessor));
    assert!(has(&lines[4], "/* open", K::Comment));
    assert!(has(&lines[5], "fn */", K::Comment));
    assert_eq!(kind_of(&lines[6], "~~~"), Some(K::Operator), "shorter fence does not close");
    assert!(has(&lines[7], "~~~~", K::Code));
    assert!(has(&lines[8], "[link](http://a.b)", K::Link));
}

#[test]
fn csv_columns_cycle_and_respect_quotes() {
    let n = usize::from(RAINBOW_COLUMNS);
    let row: Vec<String> = (0..n + 2).map(|i| format!("c{i}")).collect();
    let text = format!("a,\"b,still b\",c\n{}\n\"multi\nline\",next", row.join(","));
    let lines = lex("csv", &text);
    assert_eq!(
        lines[0],
        vec![
            ("a".to_string(), K::CsvColumn(0)),
            ("\"b,still b\"".to_string(), K::CsvColumn(1)),
            ("c".to_string(), K::CsvColumn(2)),
        ]
    );
    assert!(has(&lines[1], &format!("c{n}"), K::CsvColumn(0)));
    assert!(has(&lines[1], &format!("c{}", n + 1), K::CsvColumn(1)));
    assert!(has(&lines[3], "line\"", K::CsvColumn(0)));
    assert!(has(&lines[3], "next", K::CsvColumn(1)));
    let tsv = lex("tsv", "a,b\tc");
    assert!(has(&tsv[0], "a,b", K::CsvColumn(0)));
}

#[test]
fn xml_tags_attributes_comments_cdata() {
    let text = "<a href=\"x\" id='y'>t &amp; <!-- c\nc --><![CDATA[ <b> ]]></a>";
    let lines = lex("xml", text);
    assert!(has(&lines[0], "<a", K::Tag));
    assert!(has(&lines[0], "href", K::Attribute));
    assert!(has(&lines[0], "\"x\"", K::String));
    assert!(has(&lines[0], "&amp;", K::Escape));
    assert!(has(&lines[0], "<!-- c", K::Comment));
    assert!(has(&lines[1], "c -->", K::Comment));
    assert!(has(&lines[1], " <b> ]]>", K::String));
}

#[test]
fn ini_toml_yaml_json_keys() {
    assert!(has(&lex("ini", "[core]\nname = value ; c")[1], "name", K::Attribute));
    assert!(has(&lex("ini", "[core]")[0], "[core]", K::Heading));
    let toml = lex("toml", "[[bin]]\nkey = \"\"\"\n[not.section]\n\"\"\"");
    assert!(has(&toml[0], "[[bin]]", K::Heading));
    assert!(has(&toml[1], "key", K::Attribute));
    assert!(has(&toml[2], "[not.section]", K::String));
    let yaml = lex("yaml", "- name: it's # comment\n  url: http://x");
    assert!(has(&yaml[0], "name", K::Attribute));
    assert!(has(&yaml[0], "# comment", K::Comment));
    assert!(has(&yaml[1], "url", K::Attribute));
    let json = lex("json", "{\"k\": \"v\", \"n\": true}");
    assert!(has(&json[0], "\"k\"", K::Attribute));
    assert!(has(&json[0], "\"v\"", K::String));
    assert!(has(&json[0], "true", K::Constant));
}

#[test]
fn diff_lines() {
    let lines = lex("diff", "--- a/x\n+++ b/x\n@@ -1 +1 @@\n-old\n+new\n same");
    assert_eq!(kind_of(&lines[0], "a/x"), Some(K::Heading));
    assert_eq!(kind_of(&lines[2], "@@"), Some(K::Preprocessor));
    assert_eq!(kind_of(&lines[3], "old"), Some(K::Constant));
    assert_eq!(kind_of(&lines[4], "new"), Some(K::String));
    assert!(lines[5].is_empty());
}

const CORPUS: &[&str] = &[
    "int main(void) { /* c */ return 0x1F + 1.5e-3; } // x",
    "#include <stdio.h>\n#define X \"a\\\"b\"",
    "fn x<'a>(s: &'a str) -> r#\"raw\"# { b'\\x41'; /* /* n */ */ }",
    "def f(x=r'\\d', *a): \"\"\"doc\n more\"\"\" ; '''\nend",
    "auto s = R\"d(raw)\" )d\"; u8\"x\"; L'c';",
    "const t = `tmpl ${x}\nline`; let $y = /re/g; @decorator",
    "{\"a\": [1, 2.0, -3e5, null], \"b\\u00e9\": \"\\uD83D\"}",
    "<?xml version=\"1.0\"?><!DOCTYPE x><r a=b c='d\ne'><![CDATA[x]]><!-- y --></r>",
    "# H1\n```py\nx = 1\n```\n*em* **st** _u_ `c` [l](u) ![i](s) <http://x> {{< sc >}} ::: {.c}",
    "---\ntitle: x\n---\n<!-- c\nc -->\n> quote\n1. item\n- [ ] task",
    "[sec]\nkey = \"v\" ; c\n# c\n[[arr]]\nk.v = '''\nx'''",
    "key: value\n- a: 'b' # c\n  c: \"d\\n\"\n--- \n%YAML 1.2\n&anc *ali !!str",
    "#!/bin/bash\necho \"$x ${y} $(z)\" '$q' `cmd` $@ # c",
    "$a = @\"\nx $b `n\n\"@\n<# block\n#> [string]$c -eq 'x' `t",
    "@echo off\nset A=%B%!C!%%i %1 ^& rem x\n:: c\n:lbl",
    "SELECT * FROM t WHERE a = 'x''y' AND @v -- c\n/* b */",
    "x <- c(1, NA, TRUE) # c\nf <- function(a) { \"s\" }",
    "local t = { [[long\nstring]] } --[[ block\ncomment ]] -- c",
    "all: main.o\n\t$(CC) -o $@ $^\nCC := gcc\nifeq ($(X),1)",
    "cmake_minimum_required(VERSION 3.10)\nset(X ${Y} \"$ENV{Z}\") #[[ b\nc ]] # c",
    "diff --git a/x b/x\n@@ -1 +1 @@\n-a\n+b\n\\ No newline",
    "a,\"b,\"\"c\"\"\",d\n\"open\nclose\",x\te",
    "a { color: #fff; margin: .5em !important; } @media screen { .c:hover::before { } }",
    "class C<T> : I { [Attr] public string S => @\"v\"\"x\"; $\"i{x}\" #region r }",
    "package main; func f() (int, error) { return `raw\n`, nil }",
    "public @interface A { String s() default \"\"\"\ntext\"\"\"; char c = '\\''; }",
    "émoji 🎉 ünïcödé \u{200b} 'ü' \"ß\" /* ✓ */ // ∑ # ¿ <ä b=\"ö\"> %ü% $ü",
    "\\\"'`{}[]()<>#@$%^&*-+=|;:,./?~!",
    "\"unterminated 'single `back /* open <!-- open ``` [[ R\"( r#\"",
];

/// Deterministic variations: every snippet, joined pairs, and pseudo-random byte splices cut to char boundaries.
fn corpus() -> Vec<String> {
    let mut out: Vec<String> = CORPUS.iter().map(|s| s.to_string()).collect();
    for (i, a) in CORPUS.iter().enumerate() {
        out.push(format!("{a}{}", CORPUS[(i * 7 + 3) % CORPUS.len()]));
    }
    let all: String = CORPUS.concat();
    let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
    for _ in 0..300 {
        let mut s = String::new();
        for _ in 0..6 {
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            let start = (seed >> 33) as usize % all.len();
            let len = (seed >> 13) as usize % 24;
            let start = (start..all.len()).find(|&p| all.is_char_boundary(p)).unwrap_or(all.len());
            let end = (start + len).min(all.len());
            let end = (end..=all.len()).find(|&p| all.is_char_boundary(p)).unwrap_or(all.len());
            s.push_str(&all[start..end]);
        }
        out.push(s);
    }
    out
}

fn check_tokens(id: &str, line: &str, tokens: &[Token]) {
    let mut prev_end = 0;
    for t in tokens {
        assert!(t.range.start < t.range.end, "{id}: empty token {t:?} in {line:?}");
        assert!(t.range.start >= prev_end, "{id}: overlapping/unsorted {t:?} in {line:?}");
        assert!(t.range.end <= line.len(), "{id}: out of bounds {t:?} in {line:?}");
        assert!(line.is_char_boundary(t.range.start) && line.is_char_boundary(t.range.end), "{id}: {t:?} {line:?}");
        prev_end = t.range.end;
    }
}

#[test]
fn token_invariants_hold_for_every_language() {
    let texts = corpus();
    let mut foreign_states = vec![LineState::default()];
    for l in languages() {
        for text in &texts {
            let mut state = LineState::default();
            for line in text.split('\n') {
                let (tokens, next) = highlight_line(l, state, line);
                check_tokens(l.id, line, &tokens);
                state = next;
                foreign_states.push(next);
            }
            compute_folds(l, text);
        }
    }
    foreign_states.sort_by_key(|s| format!("{s:?}"));
    foreign_states.dedup();
    for l in languages() {
        for &state in &foreign_states {
            for line in CORPUS.iter().flat_map(|t| t.split('\n')) {
                let (tokens, _) = highlight_line(l, state, line);
                check_tokens(l.id, line, &tokens);
            }
        }
    }
}

#[test]
fn very_long_adversarial_lines_finish() {
    let patterns = ["[", "*a", "`", "_x ", "/*", "'", "\"\\", "<a ", "%a", "$", "r#", "{", "&#", ","];
    for l in languages() {
        for p in patterns {
            let line = p.repeat(60_000 / p.len());
            let (tokens, _) = highlight_line(l, LineState::default(), &line);
            check_tokens(l.id, &line, &tokens);
        }
    }
}

#[test]
fn cache_extends_lazily_and_invalidates() {
    let mut lines = ["int a;".to_string(), "int b;".to_string(), "int c;".to_string()];
    let mut cache = HighlightCache::new(lang("c"));
    assert_eq!(cache.language().id, "c");
    let mut requested = Vec::new();
    let s2 = cache.state_for_line(2, &mut |n| {
        requested.push(n);
        lines[n as usize].clone()
    });
    assert_eq!(requested, vec![0, 1]);
    assert_eq!(s2, LineState::default());
    lines[1] = "/* open".to_string();
    cache.invalidate_from_line(1);
    let s2 = cache.state_for_line(2, &mut |n| lines[n as usize].clone());
    assert_eq!(s2, highlight_line(lang("c"), LineState::default(), "/* open").1);
    assert_ne!(s2, LineState::default());
}

fn folds(id: &str, text: &str) -> Vec<(u64, u64)> {
    compute_folds(lang(id), text).iter().map(|FoldRange { start_line, end_line }| (*start_line, *end_line)).collect()
}

#[test]
fn folds_c_braces_ignore_strings_and_comments() {
    let text = "int f() {\n  char *s = \"{\"; // }\n  if (x) {\n    y();\n  }\n  /* { */\n}\nint g() { }";
    assert_eq!(folds("c", text), vec![(0, 6), (2, 4)]);
}

#[test]
fn folds_python_indentation() {
    let text = "def f():\n    x = 1\n\n    if x:\n        y()\n    s = \"\"\"\nnot indented\n\"\"\"\nz = 2\n";
    assert_eq!(folds("python", text), vec![(0, 7), (3, 4), (5, 7)]);
}

#[test]
fn folds_xml_tags() {
    let text = "<root>\n  <a x=\"<b>\">\n    <self/>\n    <br>\n  </a>\n  <!-- <c>\n  -->\n</root>";
    assert_eq!(folds("xml", text), vec![(0, 7), (1, 4)]);
    assert_eq!(folds("html", "<div>\n<br>\n<img src=x>\n</DIV>"), vec![(0, 3)]);
}

#[test]
fn folds_markdown_headings_and_fences() {
    let text = "# A\ntext\n## B\n```\n# not heading\n```\n\n# C\nend\n";
    assert_eq!(folds("markdown", text), vec![(0, 5), (2, 5), (3, 5), (7, 8)]);
}

#[test]
fn folds_ini_sections() {
    assert_eq!(folds("ini", "[a]\nx=1\n\n[b]\ny=2\n"), vec![(0, 1), (3, 4)]);
}

#[test]
fn language_for_path_by_extension_and_name() {
    let id = |p: &str| language_for_path(Path::new(p)).id;
    assert_eq!(id("main.RS"), "rust");
    assert_eq!(id("dir/report.qmd"), "markdown");
    assert_eq!(id("analysis.Rmd"), "markdown");
    assert_eq!(id("x.ps1"), "powershell");
    assert_eq!(id("run.CMD"), "batch");
    assert_eq!(id("data.tsv"), "tsv");
    assert_eq!(id("Makefile"), "makefile");
    assert_eq!(id("CMakeLists.txt"), "cmake");
    assert_eq!(id("notes.txt"), "plain");
    assert_eq!(id("/home/u/.bashrc"), "shell");
    assert_eq!(id("Cargo.lock"), "toml");
    assert_eq!(id("unknown.zzz"), "plain");
    assert_eq!(id("noext"), "plain");
    assert_eq!(languages()[0].id, "plain");
}

#[test]
fn language_for_fence_tags() {
    let id = |t: &str| language_for_fence(t).map(|l| l.id);
    assert_eq!(id("python"), Some("python"));
    assert_eq!(id("{python}"), Some("python"));
    assert_eq!(id("{r, echo=FALSE}"), Some("r"));
    assert_eq!(id("{.bash}"), Some("shell"));
    assert_eq!(id("PowerShell"), Some("powershell"));
    assert_eq!(id("js title=\"x\""), Some("javascript"));
    assert_eq!(id(""), None);
    assert_eq!(id("nolang"), None);
}

#[test]
fn comment_syntax_for_toggle() {
    assert_eq!(lang("rust").line_comment(), Some("//"));
    assert_eq!(lang("python").line_comment(), Some("#"));
    assert_eq!(lang("batch").line_comment(), Some("REM "));
    assert_eq!(lang("xml").line_comment(), None);
    assert_eq!(lang("markdown").block_comment(), Some(("<!--", "-->")));
}
