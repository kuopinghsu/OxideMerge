# Optional analysis policies and limits

The [English user guide](user-guide.adoc) and [Traditional Chinese user guide](user-guide.zh-TW.adoc)
describe the same optional actions and CLI flags. Every analysis report has `version: 1` and uses
an envelope separate from existing comparison reports. Standard comparisons remain unchanged.

- Merge preview uses the existing three-way text/folder engines. Text verification compares saved
  bytes and rechecks available sources. Unresolved conflicts always fail verification. Unknown
  source freshness is a warning. Folder verification checks every scoped planned/retained output
  and source fingerprint; planning fingerprints sources before and after the existing planner.
  Folder plans do not execute operations and are not transactions. Manually resolved conflicts
  are never auto-certified: output differing from the automatic preview fails the equality check.
  Merge plans also warn, without failing mechanically, when the two sides edit lines at most two
  lines apart (`nearby_edits`) or both change the same `name = value`, `name: value` or
  `"name": value` setting in different places (`shared_setting`); a deletion review is raised only
  where base lines are deleted.
- JSON objects are unordered; arrays are ordered. Duplicate keys fail. Numeric spellings are
  retained exactly (for example `1` and `1.0` can differ). Missing is distinct from null. XML uses
  expanded namespaces; attributes are unordered; child order, whitespace, comments and mixed
  content matter. DTDs/external entities/XML 1.1 are unsupported. Source spans refer to decoded
  UTF-8, not encoded bytes.
- Array elements are compared by position. When two arrays hold exactly the same elements in a
  different order, the elements out of order are reported as `moved` (with `from`, the old path);
  the longest run kept in order stays unmoved. With an array matching key (`--array-key id`, or
  *Array matching key* in the semantic view), object elements are paired by that member's value
  when every element of both arrays has a unique scalar value for it; otherwise position is used,
  and for an array of objects a report `notes` entry says so (at most 20 notes).
  The GUI groups changes into a tree of their paths.
- Each change has a `confidence`: `parser_exact` when its path alone pairs it, `array_key`,
  `reordered` (an exact reordering) or `position` (arrays of different lengths compared by
  position) for changes inside paired arrays. `ambiguous: true` marks a change another reading
  explains as well: a moved element that is in some longest run kept in order (so it may as well
  have stayed, as in a swap of two), a moved element with duplicates in its array, and every
  change found by position across a length change. Ambiguous changes need manual review.
  Fixed-seed fuzzing checks that reformatting and reordering object members compare identical,
  that a one-value edit reports exactly that path, that results are symmetric, and that text
  `serde_json` rejects is never reported identical.
- TOML (`toml` crate) and YAML (`yaml-rust`, one document) compare as JSON-shaped values: tables
  and mappings are unordered, numbers compare by value (`1_000` = `1000`, `0x10` = `16`, `1` =
  `1.0` = `1e0`, `-0.0` = `0`; every number compares by its exact decimal value, not as a
  64-bit float, so `1e23` = `100000000000000000000000`, `1e400` = `10e+399`, and `1e-400` is not
  `0`; YAML integers too large for 128 bits compare by their digits, while TOML integers must fit
  64 bits as the TOML specification requires), and
  comments and layout are ignored. TOML date-times with an offset compare by the moment they name
  (`10:00:00+02:00` = `08:00:00Z`), and local times compare with their seconds written out
  (`07:32` = `07:32:00.0`). YAML plain scalars follow the YAML 1.2 core schema, so
  `yes`/`no`/`on`/`off` are strings, and plain mapping keys compare by value the same way
  (`0x10:`, `16:` and `"16":` are one key, so a mapping holding two of them is an error; a quoted
  `"0x10":` stays `0x10`). An empty YAML
  document is `null`. `!!int`, `!!float`, `!!bool` and `!!null` must fit their value (`!!int abc`
  and `!!float 0x10` are errors), and `!!str` or `!` makes any scalar a string. TOML and YAML input is limited to 4
  MiB, 64 levels and 100,000 nodes; the YAML parser is driven event by event so nesting
  never recurses. Aliases, several documents, complex keys, tags on keys other than `!!str`, tags on
  mappings or sequences other than `!!map`, `!!seq` and `!`, and other tags are refused with an
  error, so the CLI returns 2.
- Each change is located on both sides: the path where the node is, or, on the side that lacks it,
  where it would go (for an array element, one past the counterpart of the nearest earlier paired
  element). `from` of a moved change is its first-side path, so paths inside key-paired or
  reordered arrays name the element on each side, not the second side's position.
- Copying a value in the semantic view (GUI only) is a verified edit. The expected result is the
  target tree with that one node replaced, inserted or removed; candidate splices of the target
  text (the source span re-indented to the target line or column, the bare scalar literal,
  whole-line or comma-aware removal, insertion before or after a sibling, into an empty `{}`, `[]`
  or element) are re-parsed in order, and the first whose structure equals the expected tree is
  kept. When none does, or the report is stale, nothing is written and the user edits the text.
  Moves are refused because they reorder the whole array. Only the value's text changes; comments
  and layout elsewhere stay byte for byte. A copy is one undo step and the file is written only
  on save. Fixed-seed fuzzing copies every change until two JSON files agree, with no refusals;
  for YAML, refusals must stay under a tenth of the copies.
- Three-way semantic comparison (`--semantic left base right`, or *Semantic* in a three-way
  compare, where the middle pane is the base) compares base→left and base→right and groups the
  changes by base path: paths that are prefixes of one another form one entry. An entry is `left`
  or `right` when only that side changed it, `both` when both made the same change (same kind,
  base location and new value, and the same destination for moves), and `conflict` otherwise.
  Changes to different members of one object are separate entries and do not conflict. *Use left*
  / *Use right* copy that side's value into the base pane with the verified edit above; the base
  is compared live, so there is no apply-all. Parse errors name the side (`Left:`, `Base:`,
  `Right:`); the CLI exits 2 on errors, 1 when any entry exists and 0 when none does.
- The folder option *Treat JSON, XML, TOML and YAML files with the same structure as identical*
  (`--structured`; off by default) applies only to the *Full contents* method. A file pair whose
  text differs, with a semantic format known from the extension, is parsed and counts as identical
  when no semantic change is found. Text comparison rules do not apply to that check. Folder
  synchronization still hashes such files, so a file whose bytes differ is copied.
- Review version 2 extends the existing ZIP manifest; version 1 defaults to unreviewed, and future
  versions fail. SHA-256 entry digests detect accidental/corrupt captured-content changes, not
  authenticity. Reviewer names are free-form. State edits do not mutate captured source content.
  Stale decisions count as needing action. Fingerprints include paths, content, comparison options
  and trusted converter identities; converted content may require renewed review on reopen.
  A decision also records a path-free content print. When reviewed content reappears under one new
  path with no decision of its own, the decision moves there and notes where it came from; when
  several new paths hold that content, or several old decisions claim it, each copy is marked
  stale for review again. Opening an archive refuses paths that leave it, unlisted, missing or
  link entries, oversized manifests, notes or entry counts, and versions from a newer OxideMerge.
- Baselines capture at most 256 MiB / 10,000 entries, with 16 MiB per file. Filters use existing
  folder filter semantics. Git refs resolve to immutable commits and exclude working-tree changes;
  Git filters remain trusted as documented in SECURITY.md. Exclusions are listed, not identical.
  Each capture records when it was read (UTC); the time is not part of its fingerprint. Moves
  require a unique exact digest match. A rename with edits is suspected when a deleted and an
  added text file with the same extension (each at most 256 KiB) share at least 50% and at
  least 3 of their non-blank lines and are each other's clear best match; ties stay
  added/deleted, and more than 250,000 candidate pairs skips the search (listed under
  limitations). *Not a rename* in the panel, or `--split-rename PATH` (with `/` or `\` between
  folders), keeps an added file added; the summary lists `split_renames`, and a path that is not
  an added file of the comparison is reported on standard error. `--split-rename` and
  `--important-path` need `--impact`, `--release-report` or `--baseline-left/--baseline-right`.
  An important path fragment flags a change when it is in the path or, for a move or rename, in
  the old path. The panel shows counts per category as filters, a *High priority* list of changes
  with risk flags, and each suspected rename with its share of matching lines.
- Impact indexes are keyed by SHA-256. They index only the selected captured root/filter and do
  not follow file links or execute source code. Every finding includes line, digest and evidence.
  Delimited occurrences are heuristic; matching a name proves neither dependency nor runtime
  behavior. Changed keys of JSON, TOML and YAML files are searched for as the leaf key and, for
  nested keys, as the dotted path (`db.timeout`). Findings labelled exact come from an import
  statement that resolves to exactly a changed, deleted or renamed path: relative JavaScript and
  TypeScript `import`/`export … from`/`require`/`import x = require(…)` (a `.js` specifier also
  matches the `.ts`/`.tsx` source it is compiled from), Python `import` and `from … import` (from
  the root, `src/` or relative), C/C++ `#include "…"`, Objective-C `#import "…"`, CSS `@import`,
  Rust `mod name;` and
  `use crate|super|self::…` (resolved from the nearest folder with `lib.rs` or `main.rs`), plus
  JSON `$ref` file references (rule `import-reference-v3`). JavaScript/TypeScript, Python, C/C++
  and Rust statements are read from Tree-sitter syntax trees (`use` trees up to 64 levels deep); a
  `.h` header is read as C, or as C++ when only the C++ grammar reads it without errors. A file
  with error or missing nodes, including valid code the grammar does not cover (such as Rust
  `dyn 'a + Fn(…)`), is listed as partly unassessed. The `<script>` blocks of Vue/Svelte
  components (not their template or style), Objective-C, CSS and JSON statements are read from
  tokens that skip comments and keep strings whole (up to 512
  tokens per statement, 2,000,000 per file, beyond which the file is listed as partly
  unassessed). Either way a statement may span lines and comments and strings are not code.
  Conditional, generated or macro-built imports can be missed; an exact finding replaces
  heuristic ones on its line.
- For changed or deleted JavaScript/TypeScript, Python, C/C++ and Rust files, top-level
  definitions (functions, classes/structs/enums/traits/interfaces/typedefs, and constants or
  module-level assignments; C/C++ global variables are not compared) of the old and new bytes are
  compared by the leaves of their syntax trees, so comments and layout do not count. Rust
  attributes such as `#[derive(…)]` count as part of the definition they precede, and a name
  defined more than once changes when any of its definitions does. A file importing a changed or
  removed definition by name from the changed
  file gets an exact `function_reference`, `type_reference` or `value_reference` finding (rule
  `symbol-reference-v2`) at the first use of the name it is imported as (not a member such as
  `obj.name` or an object key of the same name), or at the import when it is not used. Names of at least four characters are also searched for in other files of the same
  language as heuristic `symbol_name_reference` findings, at their first use as a name (not in a
  comment, string or import) and not in files with the exact finding. Methods, names reached
  through a namespace import and C/C++ headers (which include all names) give heuristic findings
  only. A Rust `use` tree gives one import finding per changed file it imports from.
- Name and leaf configuration key occurrences together are listed up to 500, after findings on a
  line that already has an exact finding are dropped and several reasons for one line are joined
  into one finding (a path or key path first; its `limitations` name the others); the rest are counted
  in a report `notes` entry instead of failing the analysis, so common keys such as `version` in a
  broad change leave room for the other findings, which still fail beyond 2,000.
  Unsupported syntax/symbols are unassessed. Findings can be dismissed without writes, and in a
  folder compare *Review* opens the evidence file in the review workspace.
- Semantic (JSON, XML, TOML, YAML) parser limits are 64 levels / 100,000 nodes; impact stops at
  2,000 findings and reads files of up to 1 MiB in parallel. UI jobs can be
  cancelled, but a bounded parse or merge is interrupted only before/after the operation. Both
  baselines and an impact text index occupy memory; the limits apply per capture, not total RSS.
- Report output cannot replace an input (including a hard-link alias) or appear inside a selected
  input directory. Report HTML/XML escape all embedded data. JSON/XML/HTML reports are snapshots
  of analysis, not executable instructions or cryptographically authenticated attestations.

## Tree view

The Tree view (*Tree* beside *Text* and *Semantic* for two JSON, YAML, TOML or XML files) is a
read-only presentation built from the same parsers, limits and comparison rules as the semantic
adapters; it never re-serializes or writes either file. Values therefore compare exactly as in
the Semantic view: for JSON, YAML and TOML, key order, comments, quoting and flow or block style
do not count, and YAML and TOML numbers compare by value (`1` equals `1.0`) while JSON keeps
numeric spellings (`1` and `1.0` differ); for XML, attributes are unordered and child order, text whitespace and comments
count.

- **Projection.** Each document becomes nodes with per-load ids (not persistent across reloads),
  kind, ordered children, parent, byte span, 1-based line and column. JSON, YAML and TOML give
  objects, arrays and scalars: members keep the order they are written in (by the start of their
  value) and their span starts at their key, array elements their index, and a scalar keeps its type (`string`, `number`, `bool`,
  `null`, `datetime`). XML gives elements with their expanded name `{uri}local`, the name as
  written with its prefix and attributes in source order, plus text, comments and processing
  instructions. Paths are JSON-shaped (`$.server.port`, `$.ports[1]`, `$["a b"]`) or XPath-shaped
  (`/catalog/book[2]/@id`); selections are carried to a new comparison by path, and when that path
  is gone the view says so.
- **Pairing** runs below paired parents only (no cross-parent moves). Object members and XML
  attributes pair by key or expanded name, never as moves, in the order of the second file with
  those only the first has after their predecessor; two members of different types are paired and
  reported as a type change. Array elements and XML child nodes are sibling groups, paired by:
  (1) key rules `name@key[+key]` — for JSON, YAML and TOML, `name` is the key of the array, a path
  of keys (`spec/ports`) or `*`/`$` (which also cover a top-level array; only `*` covers an array
  directly inside another, which has no key) and the keys are scalar members of its objects; for XML, `name` is an element's local name or a path ending in it, the
  keys are attributes, and only elements of the same expanded name pair. A rule is used for a
  group only when every covered node on both sides has a complete and unique key — otherwise a
  warning counts duplicates and missing keys and the covered nodes fall back and are marked
  ambiguous. Without a rule, the objects of an array are paired by the first of `id`, `name` and
  `key` that every element on both sides holds as a scalar with a unique value, when at least one
  value is shared; the toolbar counts these by member name. Then (2) a longest common subsequence of identical subtrees; (3) identical
  subtrees occurring once on each side, out of order, as moves; (4) between those anchors, an
  order-preserving pairing by kind and name, marked ambiguous when the group holds unpaired
  siblings of the same kind and name. Key-paired nodes out of the longest increasing order are
  moves, ambiguous when they belong to some longest order (a swap). An addition beside an
  identical paired sibling is ambiguous too.
- **Counts** are of aligned rows: an added or removed subtree is one change, a changed scalar,
  attribute, text, comment or processing instruction is one, and a move is one. They can be lower
  than the path-by-path semantic report, which compares by position. Both agree on whether the
  documents are identical. In XML, unchanged whitespace-only text rows are hidden; changed ones are
  shown, except whitespace added or removed next to an added or removed element (its indentation),
  which is part of it.
- **Search** matches keys, names, values, attribute values, text and comments. For JSON, YAML and
  TOML a query starting with `$` selects a path and everything below it, by whole steps. For XML a
  query starting with `/` is an XPath subset (`/`, `//`, names, `*`, `@name`, `text()`,
  `comment()`, `[@a]`, `[@a='v']`, `[n]`, at most 32 steps); other expressions are refused.
- **Collections** tabulate the objects of an array by their scalar members, or repeated XML child
  elements by attribute, by text-only child element (grouped by expanded name, with that child's
  attributes as `child/@attr` columns) and by text when they hold only text. Nested objects and
  arrays, and deeper or mixed XML content, are flagged, not flattened.
- Building runs on a background thread with cancellation; rows are drawn virtualized. Key rules
  apply to this view only and change neither `--semantic` output nor reports.

## Still outside this implementation

Type-aware dependency analysis (a name is matched to what a file imports by name, not resolved
through types, methods or re-exports), semantic path ignore rules, moving array elements or
merging every structured change at once, and cryptographic reviewer identity are not provided. Folder auto merge checks every item before
writing, keeps a copy of everything it replaces or deletes in a temporary folder, and undoes its
changes when one fails or it is stopped; a crash or power loss during the merge can still leave
it partly applied, with per-file atomic saves and backups as before.
Git checkout/export cancellation waits for the existing trusted Git provider to return; capture
limits are checked after export. External Git filters may execute/download as explained in
SECURITY.md. No native-language dependency graph or API compatibility rules are implemented.
New controls have Traditional Chinese translations and explicit tested English text in the other
bundled language packs until native translations are supplied.
See [benchmarks](benchmarks.md) for comparison-engine measurement methods and recorded results.

## Export and captured comparisons

Release HTML has stable per-file anchors and bounded, sandboxed detail comparisons produced by
the existing HTML renderer (up to 100 small text files, 128 KiB per input and 16 MiB of detail).
Omitted previews are labeled explicitly; full metadata/classifications remain in JSON. All analysis
report output is capped at 32 MiB. Captured binary/image comparisons use the existing views with
private temporary copies; unlocking one changes only that private copy, not the source baseline.
The start page accepts both baseline source specifications directly, including two Git refs of the
same repository, without requiring two distinct live folders first.

## Resource and scope details

The baseline panel exposes the existing folder filter syntax. For example,
`*;!target/;!node_modules/` excludes generated directories. Unknown named filters fail explicitly.
Intentional filter exclusions remain visible as outside scope; unreadable or unsupported captured
paths make the CLI return 2. A clean result only covers the selected scope.

Semantic source navigation supports LF, CRLF and CR physical lines. Changes are paginated in the
GUI and displayed values are abbreviated; exports retain bounded full values. XML expanded names
are capped at 1 KiB and semantic paths at 4 KiB, with a 32 MiB change budget. The impact cache
bounds indexed lines to 100,000 per file and 1,000,000 total, with a 256 MiB estimated footprint;
lines longer than 4 KiB are explicitly omitted. Multi-pattern matching has a 1 MiB pattern budget,
32 MiB matcher budget and 10 million match-step limit. Cancellation/error clears partial caches.

Actual folder merge confirmation now hashes regular files with SHA-256, in addition to checking
metadata, so same-size edits with restored timestamps cannot silently pass the stale-input check.
This adds file-reading cost to explicit plan/confirmation operations; existing engine benchmarks
do not measure that cost or the new semantic/impact analyses.
