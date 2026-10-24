//! Comment syntax of common languages, for ignoring changes to comments when comparing.

use std::path::Path;

pub struct CommentSyntax {
    line: &'static [&'static str],
    block: &'static [(&'static str, &'static str)],
    /// Characters that open and close a string, inside which nothing is a comment.
    quotes: &'static [char],
    /// `'x'` is a character literal (C family), but a lone `'` (a Rust lifetime) is not a quote.
    char_literals: bool,
}

const C: CommentSyntax = CommentSyntax {
    line: &["//"],
    block: &[("/*", "*/")],
    quotes: &['"'],
    char_literals: true,
};
const SCRIPT: CommentSyntax = CommentSyntax {
    line: &["//"],
    block: &[("/*", "*/")],
    quotes: &['"', '\'', '`'],
    char_literals: false,
};
const CSS: CommentSyntax = CommentSyntax {
    line: &[],
    block: &[("/*", "*/")],
    quotes: &['"', '\''],
    char_literals: false,
};
const HASH: CommentSyntax = CommentSyntax {
    line: &["#"],
    block: &[],
    quotes: &['"', '\''],
    char_literals: false,
};
const INI: CommentSyntax = CommentSyntax {
    line: &[";", "#"],
    block: &[],
    quotes: &['"'],
    char_literals: false,
};
const SQL: CommentSyntax = CommentSyntax {
    line: &["--"],
    block: &[("/*", "*/")],
    quotes: &['\'', '"'],
    char_literals: false,
};
const LUA: CommentSyntax = CommentSyntax {
    line: &["--"],
    block: &[("--[[", "]]")],
    quotes: &['"', '\''],
    char_literals: false,
};
const HASKELL: CommentSyntax = CommentSyntax {
    line: &["--"],
    block: &[("{-", "-}")],
    quotes: &['"'],
    char_literals: false,
};
const MARKUP: CommentSyntax = CommentSyntax {
    line: &[],
    block: &[("<!--", "-->")],
    quotes: &[],
    char_literals: false,
};
const POWERSHELL: CommentSyntax = CommentSyntax {
    line: &["#"],
    block: &[("<#", "#>")],
    quotes: &['"', '\''],
    char_literals: false,
};
const LISP: CommentSyntax = CommentSyntax {
    line: &[";"],
    block: &[],
    quotes: &['"'],
    char_literals: false,
};
const TEX: CommentSyntax = CommentSyntax {
    line: &["%"],
    block: &[],
    quotes: &[],
    char_literals: false,
};
const VB: CommentSyntax = CommentSyntax {
    line: &["'"],
    block: &[],
    quotes: &['"'],
    char_literals: false,
};
const FORTRAN: CommentSyntax = CommentSyntax {
    line: &["!"],
    block: &[],
    quotes: &['"', '\''],
    char_literals: false,
};
const PHP: CommentSyntax = CommentSyntax {
    line: &["//", "#"],
    block: &[("/*", "*/")],
    quotes: &['"', '\''],
    char_literals: false,
};
const TERRAFORM: CommentSyntax = PHP;

/// The comment syntax of a file, from its extension or name.
pub fn for_path(path: &Path) -> Option<&'static CommentSyntax> {
    let name = path.file_name()?.to_string_lossy().to_ascii_lowercase();
    let by_name = match name.as_str() {
        "makefile" | "gnumakefile" | "dockerfile" | "cmakelists.txt" | ".gitignore"
        | ".gitattributes" | ".dockerignore" | ".editorconfig" | ".env" => Some(&HASH),
        _ => None,
    };
    by_name.or_else(|| for_extension(&super::text::extension(path)))
}

pub fn for_extension(ext: &str) -> Option<&'static CommentSyntax> {
    Some(match ext {
        "c" | "h" | "cpp" | "cc" | "cxx" | "c++" | "hpp" | "hh" | "hxx" | "inl" | "cs" | "java"
        | "rs" | "swift" | "kt" | "kts" | "scala" | "groovy" | "gradle" | "dart" | "mm"
        | "proto" | "v" | "sv" | "svh" | "zig" | "cu" | "glsl" | "hlsl" | "jsonc" | "json5" => &C,
        "js" | "mjs" | "cjs" | "jsx" | "ts" | "mts" | "cts" | "tsx" | "go" | "scss" | "less" => {
            &SCRIPT
        }
        "css" => &CSS,
        "py" | "pyw" | "sh" | "bash" | "zsh" | "fish" | "ksh" | "rb" | "pl" | "pm" | "r"
        | "yaml" | "yml" | "toml" | "conf" | "cmake" | "mk" | "ex" | "exs" | "cr" | "nim"
        | "tcl" | "awk" | "properties" | "jl" | "coffee" => &HASH,
        "ini" | "cfg" | "inf" | "reg" | "asm" | "s" => &INI,
        "sql" | "ada" | "adb" | "ads" | "vhd" | "vhdl" => &SQL,
        "lua" => &LUA,
        "hs" | "elm" => &HASKELL,
        "xml" | "html" | "htm" | "xhtml" | "svg" | "xsl" | "xslt" | "xsd" | "xaml" | "csproj"
        | "vbproj" | "vcxproj" | "props" | "targets" | "resx" | "nuspec" | "plist" | "vue"
        | "md" | "markdown" => &MARKUP,
        "ps1" | "psm1" | "psd1" => &POWERSHELL,
        "lisp" | "lsp" | "el" | "clj" | "cljs" | "scm" | "rkt" => &LISP,
        "tex" | "sty" | "cls" | "bib" | "erl" | "hrl" => &TEX,
        "vb" | "bas" | "vbs" => &VB,
        "f" | "f90" | "f95" | "f03" | "f08" => &FORTRAN,
        "php" => &PHP,
        "tf" | "tfvars" | "hcl" => &TERRAFORM,
        _ => return None,
    })
}

/// A line with its comments removed.
pub struct Stripped {
    pub code: String,
    /// The line had comments and nothing else but whitespace.
    pub comment_only: bool,
}

/// Removes the comments from `lines`; block comments may span lines. Lines without comments
/// are kept as they are, and trailing whitespace before a removed comment is dropped.
pub fn strip<S: AsRef<str>>(lines: &[S], syn: &CommentSyntax) -> Vec<Stripped> {
    let mut in_block: Option<&str> = None;
    lines
        .iter()
        .map(|line| {
            let line = line.as_ref();
            let mut code = String::with_capacity(line.len());
            let mut had_comment = false;
            let mut i = 0;
            'line: while i < line.len() {
                let rest = &line[i..];
                if let Some(end) = in_block {
                    had_comment = true;
                    match rest.find(end) {
                        Some(k) => {
                            i += k + end.len();
                            in_block = None;
                            continue;
                        }
                        None => break,
                    }
                }
                if syn.line.iter().any(|m| rest.starts_with(m)) {
                    had_comment = true;
                    break;
                }
                for &(open, close) in syn.block {
                    if rest.starts_with(open) {
                        had_comment = true;
                        in_block = Some(close);
                        i += open.len();
                        continue 'line;
                    }
                }
                let ch = rest.chars().next().unwrap_or(' ');
                let len = if syn.quotes.contains(&ch) {
                    quoted_len(rest, ch)
                } else if syn.char_literals && ch == '\'' {
                    char_literal_len(rest)
                } else {
                    ch.len_utf8()
                };
                code.push_str(&rest[..len]);
                i += len;
            }
            if had_comment {
                let comment_only = code.trim().is_empty();
                code.truncate(code.trim_end().len());
                Stripped { code, comment_only }
            } else {
                Stripped {
                    code: line.to_string(),
                    comment_only: false,
                }
            }
        })
        .collect()
}

/// Length of the string starting at `s` (which begins with `quote`), up to and including the
/// closing quote, or to the end of the line.
fn quoted_len(s: &str, quote: char) -> usize {
    let mut escaped = false;
    for (k, c) in s.char_indices().skip(1) {
        if escaped {
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == quote {
            return k + c.len_utf8();
        }
    }
    s.len()
}

/// Length of a character literal such as `'a'` or `'\n'` at the start of `s`; 1 when the
/// quote does not start one.
fn char_literal_len(s: &str) -> usize {
    let mut chars = s.char_indices().skip(1);
    let close = match chars.next() {
        Some((_, '\\')) => chars.nth(1),
        Some(_) => chars.next(),
        None => None,
    };
    match close {
        Some((k, '\'')) => k + 1,
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codes(lines: &[&str], ext: &str) -> Vec<(String, bool)> {
        strip(lines, for_extension(ext).unwrap())
            .into_iter()
            .map(|s| (s.code, s.comment_only))
            .collect()
    }

    #[test]
    fn strips_line_and_block_comments_outside_strings() {
        let out = codes(
            &[
                "int a = 1; // one",
                "/* start",
                "   still comment */ int b;",
                "char *s = \"// not a comment\"; char c = '\"'; // x",
                "fn f<'a>(x: &'a str) {} // lifetime",
                "",
            ],
            "c",
        );
        assert_eq!(out[0], ("int a = 1;".into(), false));
        assert_eq!(out[1], ("".into(), true));
        assert_eq!(out[2], (" int b;".into(), false));
        assert_eq!(out[3].0, "char *s = \"// not a comment\"; char c = '\"';");
        assert_eq!(out[4].0, "fn f<'a>(x: &'a str) {}");
        assert_eq!(out[5], ("".into(), false));

        let out = codes(&["x = '#'  # note", "# only"], "py");
        assert_eq!(out[0], ("x = '#'".into(), false));
        assert!(out[1].1);
        let out = codes(&["<a/> <!-- c -->", "<!--", "-->"], "xml");
        assert_eq!(out[0].0, "<a/>");
        assert!(out[1].1 && out[2].1);
        assert!(for_path(Path::new("dir/Makefile")).is_some());
        assert!(for_path(Path::new("notes.txt")).is_none());
    }
}
