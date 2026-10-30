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
- JSON objects are unordered; arrays are ordered. Duplicate keys fail. Numeric spellings are
  retained exactly (for example `1` and `1.0` can differ). Missing is distinct from null. XML uses
  expanded namespaces; attributes are unordered; child order, whitespace, comments and mixed
  content matter. DTDs/external entities/XML 1.1 are unsupported. YAML/TOML and structured
  write-back are intentionally deferred. Source spans refer to decoded UTF-8, not encoded bytes.
- Review version 2 extends the existing ZIP manifest; version 1 defaults to unreviewed, and future
  versions fail. SHA-256 entry digests detect accidental/corrupt captured-content changes, not
  authenticity. Reviewer names are free-form. State edits do not mutate captured source content.
  Stale decisions count as needing action. Fingerprints include paths, content, comparison options
  and trusted converter identities; converted content may require renewed review on reopen.
- Baselines capture at most 256 MiB / 10,000 entries, with 16 MiB per file. Filters use existing
  folder filter semantics. Git refs resolve to immutable commits and exclude working-tree changes;
  Git filters remain trusted as documented in SECURITY.md. Exclusions are listed, not identical.
  Moves require a unique exact digest match; ambiguous/edited renames remain additions/deletions.
- Impact indexes are keyed by SHA-256. They index only the selected captured root/filter and do
  not follow file links or execute source code. Every finding includes line, digest and evidence.
  Delimited occurrences are heuristic; matching a name proves neither dependency nor runtime
  behavior. Unsupported syntax/symbols are unassessed. Findings can be dismissed without writes.
- Parser limits are 64 levels / 100,000 nodes; impact stops at 2,000 findings. UI jobs can be
  cancelled, but a bounded parse or merge is interrupted only before/after the operation. Both
  baselines and an impact text index occupy memory; the limits apply per capture, not total RSS.
- Report output cannot replace an input (including a hard-link alias) or appear inside a selected
  input directory. Report HTML/XML escape all embedded data. JSON/XML/HTML reports are snapshots
  of analysis, not executable instructions or cryptographically authenticated attestations.

## Still outside this implementation

Similarity-based rename matching, language parser/symbol dependency adapters, semantic path ignore
rules, array matching keys, YAML/TOML adapters, automatic structured editing, transactional folder
commit/rollback, and cryptographic reviewer identity are not provided. Existing folder merge
confirmation, stale-file checks and per-file atomic save/backup behavior remain the write path.
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
