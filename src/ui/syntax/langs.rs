//! Languages highlighted on top of the ones syntect comes with. Each is a short table of its
//! comments, strings and words, turned into a syntect syntax when the highlighter loads; a
//! file extension of a language syntect already knows is only mapped to it ([`alias`]).

use syntect::parsing::{SyntaxDefinition, SyntaxSetBuilder};

/// A string literal: its delimiter, whether a backslash escapes in it, and whether it may
/// run over several lines.
type Quote = (&'static str, bool, bool);

const DQ: Quote = ("\"", true, false);
const SQ: Quote = ("'", true, false);
/// Quoted text where a backslash is a character like any other.
const DQ_RAW: Quote = ("\"", false, false);
const SQ_RAW: Quote = ("'", false, false);
const TRIPLE_DQ: Quote = ("\"\"\"", true, true);

/// Scopes the light and the dark theme both colour.
const KEYWORD: &str = "keyword.control";
const TYPE: &str = "storage.type";
const CONSTANT: &str = "constant.language";
const NUMBER: &str = "constant.numeric";
const FUNCTION: &str = "entity.name.function";
const SECTION: &str = "entity.name.tag";
const VARIABLE: &str = "variable.parameter";
const SUPPORT: &str = "support.function";
const COMMENT: &str = "comment.line";
/// For a rule with two groups, a keyword and the name it declares: a scope for each group,
/// separated by `|`.
const KEYWORD_THEN_NAME: &str = "keyword.control|entity.name.function";
const STRING: &str = "string.quoted";

struct Lang {
    name: &'static str,
    /// File extensions in lower case; `dockerfile` is the one [`file_name_key`] gives.
    exts: &'static [&'static str],
    ignore_case: bool,
    /// What starts a comment that runs to the end of the line.
    line_comments: &'static [&'static str],
    block_comment: Option<(&'static str, &'static str)>,
    strings: &'static [Quote],
    /// Words, separated by spaces.
    keywords: &'static str,
    types: &'static str,
    constants: &'static str,
    /// Regular expressions for what the rest can't describe, with their scopes; tried
    /// before everything else.
    extra: &'static [(&'static str, &'static str)],
}

const NONE: Lang = Lang {
    name: "",
    exts: &[],
    ignore_case: false,
    line_comments: &[],
    block_comment: None,
    strings: &[],
    keywords: "",
    types: "",
    constants: "",
    extra: &[],
};

const C_BLOCK: Option<(&str, &str)> = Some(("/*", "*/"));
const PREPROCESSOR: (&str, &str) = (
    r"^\s*#\s*(?:include|define|undef|ifdef|ifndef|if|elif|else|endif|pragma|error|line)\b",
    KEYWORD,
);

const LANGS: &[Lang] = &[
    Lang {
        name: "TOML",
        exts: &["toml"],
        line_comments: &["#"],
        strings: &[TRIPLE_DQ, ("'''", false, true), DQ, SQ_RAW],
        constants: "true false inf nan",
        extra: &[
            (r"^\s*\[\[?[^\]\n]*\]\]?", SECTION),
            (r#"^\s*[\w.\-"']+(?=\s*=)"#, VARIABLE),
            (
                r"\b\d{4}-\d{2}-\d{2}(?:[Tt ]\d{2}:\d{2}:\d{2}(?:\.\d+)?)?(?:[Zz]|[+-]\d{2}:\d{2})?",
                NUMBER,
            ),
        ],
        ..NONE
    },
    Lang {
        name: "INI",
        exts: &[
            "ini",
            "cfg",
            "conf",
            "inf",
            "desktop",
            "editorconfig",
            "gitconfig",
        ],
        ignore_case: true,
        strings: &[DQ],
        constants: "true false yes no on off",
        extra: &[
            (r"^\s*[;#].*$", COMMENT),
            (r"^\s*\[[^\]\n]*\]", SECTION),
            // Up to the `=`, spaces before it included: looking for those apart would take
            // a time that grows with the square of a line of spaces.
            (r"^\s*[^=\[;#\s][^=\n]*(?==)", VARIABLE),
        ],
        ..NONE
    },
    Lang {
        name: "Dockerfile",
        exts: &["dockerfile"],
        ignore_case: true,
        strings: &[DQ, SQ],
        extra: &[
            (r"^\s*#.*$", COMMENT),
            (
                r"^\s*(?:FROM|RUN|CMD|LABEL|MAINTAINER|EXPOSE|ENV|ADD|COPY|ENTRYPOINT|VOLUME|USER|WORKDIR|ARG|ONBUILD|STOPSIGNAL|HEALTHCHECK|SHELL)\b",
                KEYWORD,
            ),
            (r"\b(?:AS)\b", KEYWORD),
            (r"\$\{[^}\n]*\}|\$\w+", VARIABLE),
            (r"--[a-z][\w-]*", TYPE),
        ],
        ..NONE
    },
    Lang {
        name: "CMake",
        exts: &["cmake"],
        ignore_case: true,
        line_comments: &["#"],
        block_comment: Some(("#[[", "]]")),
        strings: &[("\"", true, true)],
        constants: "ON OFF TRUE FALSE YES NO",
        extra: &[
            (
                r"^\s*(?:if|elseif|else|endif|foreach|endforeach|while|endwhile|function|endfunction|macro|endmacro|block|endblock|return|break|continue)(?=\s*\()",
                KEYWORD,
            ),
            (r"^\s*\w+(?=\s*\()", SUPPORT),
            (r"\$(?:ENV|CACHE)?\{[^}\n]*\}", VARIABLE),
            (
                r"\b(?-i:AND|OR|NOT|STREQUAL|STRLESS|STRGREATER|EQUAL|LESS|GREATER|MATCHES|DEFINED|EXISTS|COMMAND|TARGET|IN_LIST|VERSION_LESS|VERSION_GREATER|VERSION_EQUAL|PUBLIC|PRIVATE|INTERFACE|REQUIRED|COMPONENTS|STATIC|SHARED|MODULE|OBJECT|IMPORTED|PROPERTIES|PARENT_SCOPE|FORCE|APPEND)\b",
                TYPE,
            ),
        ],
        ..NONE
    },
    Lang {
        name: "PowerShell",
        exts: &["ps1", "psm1", "psd1"],
        ignore_case: true,
        line_comments: &["#"],
        block_comment: Some(("<#", "#>")),
        strings: &[("\"", false, true), ("'", false, true)],
        keywords: "begin break catch class continue data define do dynamicparam else elseif end \
                   enum exit filter finally for foreach from function hidden if in param process \
                   return static switch throw trap try until using var while workflow",
        extra: &[
            (r"\$(?:\{[^}\n]*\}|[\w:]+)", VARIABLE),
            (
                r"(?<![\w-])-(?:eq|ne|gt|ge|lt|le|like|notlike|match|notmatch|contains|notcontains|in|notin|replace|split|join|is|isnot|as|and|or|not|xor|band|bor|bnot|bxor|shl|shr|f)\b",
                KEYWORD,
            ),
            (r"\b[A-Za-z]+-[A-Za-z]+\b", SUPPORT),
            (r"\[[A-Za-z_][\w.]*(?:\[\])?\]", TYPE),
        ],
        ..NONE
    },
    Lang {
        name: "Kotlin",
        exts: &["kt", "kts"],
        line_comments: &["//"],
        block_comment: C_BLOCK,
        strings: &[TRIPLE_DQ, DQ, SQ],
        keywords: "abstract actual annotation as break by catch class companion const \
                   constructor continue crossinline data do else enum expect external final \
                   finally for fun get if import in infix init inline inner interface internal \
                   is lateinit noinline object open operator out override package private \
                   protected public reified return sealed set super suspend tailrec this throw \
                   try typealias val value var vararg when where while",
        types: "Any Array Boolean Byte Char Double Float Int List Long Map Nothing Set Short \
                String Unit",
        constants: "true false null",
        extra: &[(r"@\w+", SUPPORT), (r"\b(fun)\s+(\w+)", KEYWORD_THEN_NAME)],
        ..NONE
    },
    Lang {
        name: "Swift",
        exts: &["swift"],
        line_comments: &["//"],
        block_comment: C_BLOCK,
        strings: &[TRIPLE_DQ, DQ],
        keywords: "actor any as associatedtype async await break case catch class continue \
                   convenience default defer deinit didSet do dynamic else enum extension \
                   fallthrough fileprivate final for func get guard if import in indirect infix \
                   init inout internal is lazy let mutating nonisolated nonmutating open \
                   operator optional override postfix precedencegroup prefix private protocol \
                   public repeat required rethrows return self Self set some static struct \
                   subscript super switch throw throws try typealias unowned var weak where \
                   while willSet",
        types: "Any AnyObject Array Bool Character Dictionary Double Float Int Int8 Int16 Int32 \
                Int64 Never Optional Set String UInt UInt8 UInt16 UInt32 UInt64 Void",
        constants: "true false nil",
        extra: &[
            (r"[@#][A-Za-z_]\w*", SUPPORT),
            (r"\b(func)\s+(\w+)", KEYWORD_THEN_NAME),
        ],
        ..NONE
    },
    Lang {
        name: "TypeScript",
        exts: &["ts", "tsx", "mts", "cts"],
        line_comments: &["//"],
        block_comment: C_BLOCK,
        strings: &[DQ, SQ, ("`", true, true)],
        keywords: "abstract as asserts async await break case catch class const constructor \
                   continue debugger declare default delete do else enum export extends finally \
                   for from function get if implements import in infer instanceof interface is \
                   keyof let module namespace new of override package private protected public \
                   readonly return satisfies set static super switch this throw try type typeof \
                   var while with yield",
        types: "any bigint boolean never number object string symbol unknown void",
        constants: "true false null undefined NaN Infinity",
        extra: &[
            (r"@\w+", SUPPORT),
            (r"\b(function)\s+(\w+)", KEYWORD_THEN_NAME),
        ],
        ..NONE
    },
    Lang {
        name: "Verilog",
        exts: &["v", "vh", "sv", "svh"],
        line_comments: &["//"],
        block_comment: C_BLOCK,
        strings: &[DQ],
        keywords: "always always_comb always_ff always_latch assert assign assume automatic \
                   begin case casex casez class clocking constraint cover covergroup default \
                   defparam disable do else end endcase endclass endclocking endfunction \
                   endgenerate endgroup endinterface endmodule endpackage endprogram \
                   endproperty endsequence endtask enum extends final for foreach forever fork \
                   function generate genvar if iff import initial inout input inside interface \
                   join join_any join_none local localparam modport module negedge new or \
                   output package packed parameter posedge priority program property \
                   protected rand randc repeat return sequence static struct task typedef \
                   union unique virtual wait while with",
        types: "bit byte int integer logic longint real realtime reg shortint shortreal signed \
                string supply0 supply1 time tri unsigned void wand wire wor",
        constants: "null",
        extra: &[
            (
                r"\b\d*'[sS]?[bBoOdDhH][0-9a-fA-FxXzZ_?]+|'[01xXzZ]\b",
                NUMBER,
            ),
            (r"`\w+", SUPPORT),
            (r"\$\w+", SUPPORT),
        ],
        ..NONE
    },
    Lang {
        name: "VHDL",
        exts: &["vhd", "vhdl"],
        ignore_case: true,
        line_comments: &["--"],
        strings: &[DQ_RAW],
        keywords: "abs access after alias all and architecture array assert attribute begin \
                   block body buffer bus case component configuration constant context \
                   disconnect downto else elsif end entity exit file for function generate \
                   generic group guarded if impure in inertial inout is label library linkage \
                   literal loop map mod nand new next nor not null of on open or others out \
                   package port postponed procedure process protected pure range record \
                   register reject rem report return rol ror select severity shared signal sla \
                   sll sra srl subtype then to transport type unaffected units until use \
                   variable wait when while with xnor xor",
        types: "bit bit_vector boolean character integer natural positive real signed \
                std_logic std_logic_vector std_ulogic std_ulogic_vector string time unsigned",
        constants: "true false",
        extra: &[
            (r#"\b[bBoOxX]"[0-9a-fA-F_]*""#, NUMBER),
            (r"'[^'\n]'", STRING),
        ],
        ..NONE
    },
    Lang {
        name: "Assembly",
        exts: &["asm", "s", "nasm", "masm"],
        ignore_case: true,
        line_comments: &[";", "//"],
        block_comment: C_BLOCK,
        strings: &[DQ, SQ],
        constants: "al ah ax eax rax bl bh bx ebx rbx cl ch cx ecx rcx dl dh dx edx rdx si esi \
                    rsi di edi rdi sp esp rsp bp ebp rbp rip r8 r9 r10 r11 r12 r13 r14 r15 cs ds \
                    es fs gs ss pc lr fp ra gp tp zero",
        extra: &[
            PREPROCESSOR,
            (r"^\s*#.*$", COMMENT),
            (r"^\s*[A-Za-z_.$@?][\w.$@?]*:", FUNCTION),
            (r"(?<![\w.])[.%][A-Za-z_]\w*", KEYWORD),
            (
                r"\b(?:section|segment|global|globl|extern|extrn|public|db|dw|dd|dq|dt|resb|resw|resd|resq|equ|times|proc|endp|macro|endm|org|align|bits|byte|word|dword|qword|ptr|offset|end)\b",
                KEYWORD,
            ),
            (r"\b(?:0x[0-9a-f]+|[0-9][0-9a-f]*h|[01]+b)\b", NUMBER),
            (
                r"\b(?:[xw][0-9]|[xw][12][0-9]|[xw]3[01]|[rat][0-9]|s[0-9]|s1[01]|r1[0-5]|xmm\d+|ymm\d+|zmm\d+)\b",
                CONSTANT,
            ),
        ],
        ..NONE
    },
    Lang {
        name: "Protocol Buffers",
        exts: &["proto"],
        line_comments: &["//"],
        block_comment: C_BLOCK,
        strings: &[DQ, SQ],
        keywords: "syntax edition import weak public package option message enum service rpc \
                   returns stream oneof map repeated optional required reserved extensions \
                   extend group to max",
        types: "double float int32 int64 uint32 uint64 sint32 sint64 fixed32 fixed64 sfixed32 \
                sfixed64 bool string bytes",
        constants: "true false inf nan",
        ..NONE
    },
    Lang {
        name: "Windows Resources",
        exts: &["rc", "rc2", "dlg"],
        line_comments: &["//"],
        block_comment: C_BLOCK,
        strings: &[DQ],
        keywords: "ACCELERATORS AUTO3STATE AUTOCHECKBOX AUTORADIOBUTTON BEGIN BITMAP BLOCK \
                   CAPTION CHARACTERISTICS CHECKBOX CLASS COMBOBOX CONTROL CTEXT CURSOR \
                   DEFPUSHBUTTON DESIGNINFO DIALOG DIALOGEX DISCARDABLE EDITTEXT END EXSTYLE \
                   FILEFLAGS FILEFLAGSMASK FILEOS FILESUBTYPE FILETYPE FILEVERSION FIXED FONT \
                   GROUPBOX HTML ICON LANGUAGE LISTBOX LTEXT MENU MENUEX MENUITEM MESSAGETABLE \
                   MOVEABLE POPUP PRELOAD PRODUCTVERSION PURE PUSHBOX PUSHBUTTON RADIOBUTTON \
                   RCDATA RTEXT SCROLLBAR SEPARATOR STATE3 STRINGTABLE STYLE TEXTINCLUDE \
                   TOOLBAR VALUE VERSION VERSIONINFO VIRTKEY",
        constants: "NOT",
        extra: &[PREPROCESSOR],
        ..NONE
    },
    Lang {
        name: "IDL",
        exts: &["idl", "odl"],
        line_comments: &["//"],
        block_comment: C_BLOCK,
        strings: &[DQ, SQ],
        keywords: "abstract attribute case coclass component const context cpp_quote default \
                   dispinterface enum eventtype exception factory import importlib interface \
                   library local methods module native oneway properties raises readonly \
                   sequence struct switch typedef union valuetype",
        types: "any boolean byte char double fixed float hyper int long octet short signed \
                small string unsigned void wchar wchar_t wstring BSTR DWORD HRESULT LPSTR \
                LPWSTR VARIANT",
        constants: "TRUE FALSE NULL",
        extra: &[
            PREPROCESSOR,
            (
                r"\b(?:in|out|inout|retval|uuid|object|dual|oleautomation|pointer_default|propget|propput|propputref|helpstring|version|default|optional|size_is|length_is|iid_is|unique|ref|ptr|hidden|restricted|source|nonextensible|id)\b(?=[^\[\]\n]*\])",
                SUPPORT,
            ),
        ],
        ..NONE
    },
    Lang {
        name: "Visual Basic",
        exts: &[
            "vb", "bas", "frm", "cls", "ctl", "pag", "dsr", "dob", "vbs", "dsm",
        ],
        ignore_case: true,
        line_comments: &["'"],
        strings: &[DQ_RAW],
        keywords: "AddHandler AddressOf Alias And AndAlso As Attribute Begin ByRef ByVal Call \
                   Case Catch Class Const Continue Declare Dim Do Each Else ElseIf End Enum \
                   Erase Error Event Exit Finally For Friend Function Get GoSub GoTo Handles If \
                   Implements Imports In Inherits Interface Is Let Lib Like Loop Me Mod Module \
                   MustInherit MustOverride MyBase MyClass Namespace New Next Not On Option \
                   Optional Or OrElse Overloads Overridable Overrides ParamArray Preserve \
                   Private Property Protected Public RaiseEvent ReadOnly ReDim Resume Return \
                   Select Set Shadows Shared Static Step Stop Structure Sub Then Throw To Try \
                   Type TypeOf Until Wend When While With WithEvents WriteOnly Xor",
        types: "Boolean Byte Char Currency Date Decimal Double Integer Long Object Short \
                Single String Variant",
        constants: "True False Nothing Null Empty",
        extra: &[
            (r"^\s*Rem\b.*$", COMMENT),
            (r"^\s*#\s*(?:If|ElseIf|Else|End|Const|Region)\b", KEYWORD),
            (r"&H[0-9A-F]+&?", NUMBER),
            (
                r"\b(Sub|Function|Property\s+(?:Get|Let|Set))\s+(\w+)",
                KEYWORD_THEN_NAME,
            ),
        ],
        ..NONE
    },
    Lang {
        name: "Fortran",
        exts: &["f90", "f95", "f03", "f08", "f2k", "f", "for", "f77"],
        ignore_case: true,
        line_comments: &["!"],
        strings: &[DQ_RAW, SQ_RAW],
        keywords: "abstract allocatable allocate assign associate backspace block call case \
                   class close common contains continue cycle data deallocate default dimension \
                   do elemental else elseif elsewhere end endblock enddo endfunction endif \
                   endmodule endprogram endselect endsubroutine endtype endwhere entry \
                   equivalence exit extends external forall format function go goto if implicit \
                   import in include inout inquire intent interface intrinsic module namelist \
                   none nullify only open operator optional out parameter pause pointer print \
                   private procedure program public pure read recursive result return rewind \
                   save select sequence stop submodule subroutine target then to type use where \
                   while write",
        types: "character complex double doubleprecision integer logical precision real",
        extra: &[
            (r"\.(?:true|false)\.", CONSTANT),
            (r"\.(?:and|or|not|eq|ne|lt|le|gt|ge|eqv|neqv)\.", KEYWORD),
            (r"\b\d+(?:\.\d*)?[de][+-]?\d+\b", NUMBER),
            (
                r"\b(subroutine|function|program|module)\s+(\w+)",
                KEYWORD_THEN_NAME,
            ),
        ],
        ..NONE
    },
    Lang {
        name: "F#",
        exts: &["fs", "fsi", "fsx"],
        line_comments: &["//"],
        block_comment: Some(("(*", "*)")),
        strings: &[TRIPLE_DQ, DQ],
        keywords: "abstract and as assert base begin class default delegate do done downcast \
                   downto elif else end exception extern finally for fun function global if in \
                   inherit inline interface internal lazy let match member module mutable \
                   namespace new not of open or override private public rec return select \
                   static struct then to try type upcast use val void when while with yield",
        types: "bool byte char decimal double float int int16 int32 int64 list obj option \
                sbyte seq single string uint16 uint32 uint64 unit",
        constants: "true false null",
        extra: &[
            (r"^\s*#(?:if|else|endif|light|nowarn|load|r)\b", KEYWORD),
            (r"'(?:[^'\\\n]|\\.)'", STRING),
            (
                r"\b(let(?:\s+(?:rec|inline|mutable))*)\s+(\w+)",
                KEYWORD_THEN_NAME,
            ),
        ],
        ..NONE
    },
    Lang {
        name: "Standard ML",
        exts: &["sml"],
        block_comment: Some(("(*", "*)")),
        strings: &[DQ],
        keywords: "abstype and andalso as case datatype do else end eqtype exception fn fun \
                   functor handle if in include infix infixr let local nonfix of op open orelse \
                   raise rec sharing sig signature struct structure then type val where while \
                   with withtype",
        types: "bool char int list option real string unit word",
        constants: "true false nil",
        extra: &[(r#"#"(?:[^"\\\n]|\\.)""#, STRING)],
        ..NONE
    },
    Lang {
        name: "Smalltalk",
        exts: &["st", "pac"],
        block_comment: Some(("\"", "\"")),
        strings: &[("'", false, true)],
        keywords: "self super thisContext",
        constants: "true false nil",
        extra: &[
            (r"#[A-Za-z_][\w:]*|#'[^'\n]*'", CONSTANT),
            (r"\$\S", STRING),
            (r"\b[A-Za-z_]\w*:(?!=)", FUNCTION),
            (r"\^|:=", KEYWORD),
            (r"\b\d+r[0-9A-Z]+\b", NUMBER),
            (r"\b[A-Z]\w*\b", TYPE),
        ],
        ..NONE
    },
];

/// Extensions of languages syntect knows under another extension, with that extension.
const ALIASES: &[(&str, &[&str])] = &[
    ("c", &["sma"]),
    ("cpp", &["ino"]),
    ("as", &["asc"]),
    ("js", &["jsfl", "mjs", "cjs", "jsx"]),
    ("json", &["jsonc", "json5"]),
    ("pas", &["dpk", "pp", "dfm", "lpr"]),
    ("ml", &["caml"]),
    ("lisp", &["smd"]),
    ("pl", &["cgi"]),
    ("rb", &["rbw"]),
    ("scala", &["sc"]),
    ("sql", &["body", "sps", "spb", "sf", "sp"]),
    ("html", &["htd", "htt", "cfm", "hta"]),
    (
        "xml",
        &[
            "dtd", "vxml", "xsl", "xul", "axl", "xrc", "rdf", "vcproj", "vcxproj", "csproj", "wxs",
            "wxi", "docbook", "xaml",
        ],
    ),
];

/// The extension syntect knows the language of files with extension `ext` by, if it is
/// another one.
pub(super) fn alias(ext: &str) -> Option<&'static str> {
    ALIASES
        .iter()
        .find(|(_, from)| from.contains(&ext))
        .map(|&(to, _)| to)
}

/// The extension to look up a file with extension `ext` by when several languages share
/// that extension and its first line, `first`, tells which it is in; empty when the
/// extension tells nothing and only the first line can (a script's `#!`).
pub(super) fn by_first_line(ext: &str, first: &str) -> Option<&'static str> {
    let line = first.trim_start_matches('\u{feff}').trim_start();
    let lower = line.to_ascii_lowercase();
    let starts = |words: &[&str]| words.iter().any(|w| lower.starts_with(w));
    match ext {
        // A LaTeX class, not a Visual Basic one.
        "cls" if starts(&["\\", "%"]) => Some("sty"),
        // A script in whatever its first line names; Perl without one (see `ALIASES`).
        "cgi" if line.starts_with("#!") => Some(""),
        // A fragment shader, not F#.
        "fs" if starts(&[
            "#version",
            "#extension",
            "#ifdef",
            "#ifndef",
            "#define",
            "precision ",
            "uniform ",
            "varying ",
            "layout",
            "in ",
            "out ",
            "void main",
        ]) =>
        {
            Some("c")
        }
        // Included by Pascal or by an assembler, where its first line says so beyond
        // doubt; by a web page otherwise, as syntect has it.
        "inc" if starts(&["{", "(*", "unit ", "procedure ", "function "]) => Some("pas"),
        "inc" if starts(&[";"]) => Some("asm"),
        // The specification of a database package or a PyInstaller build; anything else
        // (such as the description of an RPM package) is in no language known here.
        "spec" if starts(&["create ", "package ", "--", "/*"]) => Some("sql"),
        "spec" if starts(&["# -*- mode: python"]) => Some("py"),
        "spec" => Some(""),
        _ => None,
    }
}

/// Files whose whole name tells their language, in lower case, with an extension of it.
const FILE_NAMES: &[(&str, &str)] = &[
    ("dockerfile", "dockerfile"),
    ("containerfile", "dockerfile"),
    ("cmakelists.txt", "cmake"),
    ("makefile", "makefile"),
    ("gnumakefile", "makefile"),
    ("sconstruct", "py"),
    ("sconscript", "py"),
    ("cargo.lock", "toml"),
    (".editorconfig", "ini"),
    (".gitconfig", "ini"),
    (".gitmodules", "ini"),
];

/// An extension of the language a file named `name` (in lower case) is in going by its
/// whole name, as `Dockerfile`, `Dockerfile.dev` and `CMakeLists.txt` are.
pub(super) fn file_name_key(name: &str) -> Option<&'static str> {
    if name.starts_with("dockerfile.") || name.ends_with(".dockerfile") {
        return Some("dockerfile");
    }
    FILE_NAMES
        .iter()
        .find(|(from, _)| *from == name)
        .map(|&(_, to)| to)
}

/// Adds every language of this module to `builder`. Added last, they win over a syntax of
/// syntect's with the same extension.
pub(super) fn add_to(builder: &mut SyntaxSetBuilder) {
    for lang in LANGS {
        // The definitions are part of the program: one that does not load is left out, and
        // the tests fail.
        if let Ok(def) = SyntaxDefinition::load_from_str(&definition(lang), true, None) {
            builder.add(def);
        }
    }
}

/// `text` as a YAML string.
fn quoted(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// `text` as a regular expression matching just that.
fn literal(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        if !c.is_alphanumeric() && c != '_' && c != ' ' {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// A rule giving what `regex` matches `scope`, or each of its groups one of the scopes
/// separated by `|`.
fn rule(out: &mut String, regex: &str, scope: &str) {
    out.push_str(&format!("    - match: {}\n", quoted(regex)));
    if !scope.contains('|') {
        out.push_str(&format!("      scope: {scope}\n"));
        return;
    }
    out.push_str("      captures:\n");
    for (i, scope) in scope.split('|').enumerate() {
        out.push_str(&format!("        {}: {scope}\n", i + 1));
    }
}

fn words(out: &mut String, flags: &str, words: &str, scope: &str) {
    let all: Vec<String> = words.split_whitespace().map(literal).collect();
    if !all.is_empty() {
        rule(out, &format!(r"{flags}\b(?:{})\b", all.join("|")), scope);
    }
}

/// The `.sublime-syntax` definition of `lang`.
fn definition(lang: &Lang) -> String {
    let scope = lang.name.to_lowercase().replace([' ', '#'], "-");
    let mut out = format!(
        "%YAML 1.2\n---\nname: {}\nfile_extensions: [{}]\nscope: source.{scope}\ncontexts:\n",
        quoted(lang.name),
        lang.exts
            .iter()
            .map(|e| quoted(e))
            .collect::<Vec<_>>()
            .join(", "),
    );
    let flags = if lang.ignore_case { "(?i)" } else { "" };
    let mut main = String::from("  main:\n");
    let mut pushed = String::new();
    for (regex, scope) in lang.extra {
        rule(&mut main, &format!("{flags}{regex}"), scope);
    }
    if let Some((open, close)) = lang.block_comment {
        main.push_str(&format!(
            "    - match: {}\n      push: block_comment\n",
            quoted(&literal(open))
        ));
        pushed.push_str(&format!(
            "  block_comment:\n    - meta_scope: comment.block\n    - match: {}\n      pop: true\n",
            quoted(&literal(close))
        ));
    }
    for start in lang.line_comments {
        rule(&mut main, &format!("{}.*$", literal(start)), COMMENT);
    }
    for (i, &(delimiter, escapes, multiline)) in lang.strings.iter().enumerate() {
        let delimiter = quoted(&literal(delimiter));
        main.push_str(&format!(
            "    - match: {delimiter}\n      push: string_{i}\n"
        ));
        pushed.push_str(&format!("  string_{i}:\n    - meta_scope: {STRING}\n"));
        if escapes {
            rule(&mut pushed, r"\\.", "constant.character.escape");
        }
        pushed.push_str(&format!("    - match: {delimiter}\n      pop: true\n"));
        if !multiline {
            pushed.push_str("    - match: '$'\n      pop: true\n");
        }
    }
    words(&mut main, flags, lang.keywords, KEYWORD);
    words(&mut main, flags, lang.types, TYPE);
    words(&mut main, flags, lang.constants, CONSTANT);
    rule(
        &mut main,
        r"\b(?:0[xX][0-9a-fA-F_]+|0[bB][01_]+|\d[\d_]*(?:\.\d[\d_]*)?(?:[eE][+-]?\d+)?)\b",
        NUMBER,
    );
    out.push_str(&main);
    out.push_str(&pushed);
    out
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    /// A file extension of each language, with lines of it holding the string `"x"` and a
    /// comment saying `note`, and lines starting with a keyword.
    pub const SAMPLES: &[(&str, &str, &str)] = &[
        ("toml", "name = \"x\" # note", "[package]"),
        ("ini", "name = \"x\"\n; note", "[core]"),
        (
            "dockerfile",
            "RUN echo \"x\"\n# note",
            "FROM alpine AS base",
        ),
        ("cmake", "set(NAME \"x\") # note", "if(NAME)"),
        (
            "ps1",
            "$name = \"x\" # note",
            "function Get-Name { return 1 }",
        ),
        ("kt", "val name = \"x\" // note", "fun main() { return }"),
        (
            "swift",
            "let name = \"x\" // note",
            "func main() { return }",
        ),
        (
            "ts",
            "const name = \"x\"; // note",
            "function main(): void { return; }",
        ),
        (
            "sv",
            "string name = \"x\"; // note",
            "module top; endmodule",
        ),
        ("vhd", "report \"x\"; -- note", "entity top is end entity;"),
        ("asm", "msg db \"x\" ; note", "section .text"),
        ("proto", "option name = \"x\"; // note", "message Top { }"),
        ("rc", "CAPTION \"x\" // note", "BEGIN END"),
        (
            "idl",
            "const string name = \"x\"; // note",
            "interface Top { };",
        ),
        ("vb", "name = \"x\" ' note", "Sub Main()\nEnd Sub"),
        (
            "f90",
            "name = \"x\" ! note",
            "program main\nend program main",
        ),
        ("fs", "let name = \"x\" // note", "module Top"),
        ("sml", "val name = \"x\" (* note *)", "fun main () = ()"),
        ("st", "name := 'x'. \"note\"", "^ self"),
    ];

    /// Every table loads as a syntax, under each of its extensions.
    #[test]
    fn every_language_loads() {
        let mut builder = SyntaxSetBuilder::new();
        for lang in LANGS {
            let def = SyntaxDefinition::load_from_str(&definition(lang), true, None)
                .unwrap_or_else(|e| panic!("{}: {e}\n{}", lang.name, definition(lang)));
            builder.add(def);
        }
        let set = builder.build();
        assert_eq!(set.syntaxes().len(), LANGS.len());
        assert_eq!(SAMPLES.len(), LANGS.len(), "a sample for each language");
        for (lang, (ext, ..)) in LANGS.iter().zip(SAMPLES) {
            assert!(lang.exts.contains(ext), "{}: {ext}", lang.name);
            for ext in lang.exts {
                assert_eq!(*ext, ext.to_lowercase());
                assert_eq!(alias(ext), None, "{ext} is both a language and an alias");
                let found = set.find_syntax_by_extension(ext).map(|s| s.name.as_str());
                assert_eq!(found, Some(lang.name), "{ext}");
            }
        }
    }
}
