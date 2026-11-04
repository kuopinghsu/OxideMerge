# Changelog

All notable changes to OxideMerge are listed here, newest first. Each release on GitHub uses its
section of this file as the release notes.

## [0.9.8] - Unreleased

### Added

- **Tree view:** *Tree*, beside *Text* and *Semantic* for two JSON, YAML, TOML or XML files,
  shows both documents as one aligned tree: matching nodes share a row, a node on one side only has
  a placeholder, and `+ − ~ ↕ ?` mark added, removed, modified, moved and ambiguous rows. Object
  members and attributes are paired by key or name, so key order, flow or block style, quoting and
  number spelling (`8_080`, `0x1F90`) are not changes, and a change of type is shown as such.
  Array elements and XML siblings are aligned, so inserting one is one addition rather than a
  change at every later position, and a reorder is a move. Next/previous difference visits each
  change once in tree order and unfolds collapsed parents; folded rows count the changes inside
  them under the current filter. The details pane shows each side's path, pairing, line and column
  and the source lines as written, and *Show source* opens the Text view there. Objects of an
  array are paired by a unique `id`, `name` or `key` member when every element has one, and the
  toolbar says so; key rules
  (`servers@name`, `spec/ports@name`, `*@id`, `book@id+lang`, `catalog/book@id`) pair other
  repeated entries and report reorders as moves, and duplicate or missing keys warn and fall back
  conservatively. Search by key, name or value, by a `$.path` prefix or with an XPath subset, and
  *View as collection* tables arrays of objects and repeated elements. Read-only, built in the
  background with *Cancel*, within the semantic parsers' limits. Sample: `samples/xml`.
- **Documents, spreadsheets and programs read as text**, with no other program to install:
  PDF is compared as the text of its pages, read through the file's own tables and turned back
  into characters with the `ToUnicode` table it carries for each font, so a page reads as it
  does on screen; an encrypted file says so rather than coming out as nothing, and a file whose
  tables are damaged is still read by looking for the objects themselves.
  Word (`.docx`), OpenDocument text (`.odt`) and Rich Text (`.rtf`) are compared as the words of
  the document, with the cells of a table separated by tabs; Excel (`.xlsx`, `.xlsm`) and
  OpenDocument spreadsheets (`.ods`) as their sheets, which opens as a **table compare** with
  the columns and the key matching it offers (a workbook of several sheets is compared as text);
  and ELF programs, shared objects and object files as their header, program headers (type,
  offset, addresses, file and memory sizes, alignment, `RWE` flags and the interpreter), section
  table and the symbols of `.symtab` and `.dynsym` sorted by name, with address, size, type,
  binding, visibility and section, so two builds line up function by function. Assembler mapping
  symbols (`$x`, `$d`, …) are left out, and at most 100,000 symbols a table are listed. A folder
  compare by contents still compares programs by their bytes. Text a PDF draws through forms is read too, and a PDF whose streams or fonts cannot all
  be read ends with a line saying the text is incomplete. The file itself
  is never written back: a compare read this way is read-only, and the pane says what was read.
  A converter under *Options → Plugins* is used in preference, so a better one can always be put
  in front, and *Read documents, spreadsheets and ELF files as text* there turns this off to
  compare such files as the archive or the bytes they are.
- **Formatted documents:** *Formatted*, beside *Text* above the panes, shows two or three Word
  documents, Excel workbooks (`.xlsx`, `.xlsm`) or tagged PDF files as formatted Markdown, block
  by block side by side, each changed block highlighted with its changed words. Headings,
  paragraphs with their bold, slanted and struck-through words, numbered and nested lists, and
  links keep their form. Tables are drawn as tables, with merged cells spanning the rows and
  columns they cover, so a row added inside a merged cell shows where it is. Each sheet of a
  workbook is a heading and a table, its dates, amounts and percentages shown as their number
  formats show them, runs of empty rows shown as one, and the pictures placed on it listed with
  their cells. An image or figure is shown with its description and a fingerprint of its
  picture, so a changed picture shows as a change. A PDF is read from the tags that most word
  processors write, which name its headings, lists, tables and cell spans; its running headers
  and page numbers are left out, and an untagged PDF says so. Next and previous difference step
  through the changed blocks. Fills, fonts, borders, formulas and charts are not compared.
  Samples: `samples/*/docs`.
- **Merged cells in web pages:** the Formatted web mode draws a table cell that spans rows or
  columns (`rowspan`, `colspan`) across the cells it covers, instead of shifting the cells after it
  into the wrong columns. Sample: `samples/*/web/handbook.html`.
- **Italian** interface language (`--lang it`, or *Italiano* in *Options → General*).
- A toolbar button in folder compares, and `Alt+P`, show or hide the *File compare below the folder
  list*.
- **Clear recent comparisons:** *Clear* beside the start page's *Recent* heading and *File → Open
  recent → Clear recent comparisons* empty the list, with *Undo* in the message that follows.
- **Binary editing:** click a byte in a binary compare and type hexadecimal digits, or text in the
  text column. *Insert* (or the `Insert` key) inserts bytes instead of replacing them; `Delete` and
  `Backspace` delete bytes; the arrow, page, `Home` and `End` keys move the cursor; pasting writes
  hexadecimal bytes in the hexadecimal column and text in the text column. Typing until the cursor
  moves is one undo step, and read-only sides refuse edits.
- **Table rows matched by key:** in a two-way CSV or TSV table compare, *Match rows by → Key
  columns* pairs records by one or more key columns instead of by position, *Columns by → Header
  name* pairs reordered columns, and *Numeric tolerance* (`0.01` or `1%`) counts close numbers as
  equal. A summary counts added, removed, modified, reordered and ambiguous records; next and
  previous difference go through the differing records; copying a cell or record uses the
  matching record and paired column, and records with an empty or duplicate key are never
  copied. A missing key column falls back to matching by position and says why. *Rows in*
  lists the matched rows in left, right or key order. Tables of more than 100,000 records are
  matched in the background with progress and *Cancel*. HTML and XML reports record the keys,
  column pairing, row order, tolerance, counts and warnings (a `<table-match>` element in XML).
  Projects save the settings.
- **One way through the differences of a folder:** next and previous difference go through the
  file compare below the folder list and on to the first (or last) difference of the next (or
  previous) changed file, in the order of the list, and *All differences* opens at the difference
  selected there. The status bar shows *File 2 / 5 · Difference 3 / 7*. Keys pressed while a file
  loads are taken once it is shown, a file that cannot be read offers *Retry* and *Skip*, and
  there is no wrapping around at the ends. In three-way folder compares, next and previous
  conflict go on to the files changed on both sides, passing over those that merge without
  conflicts, also from a tab opened from the folder compare. The navigation buttons' tooltips
  say what they go through and that they stop at the end, and the message at the end says when
  the folder compare was stopped before it finished.
- **Folder synchronization:** *Synchronize folders...* in a two-way folder compare (toolbar and
  *Merge* menu) previews mirroring one folder into the other, or updating it without deleting.
  The read-only preview lists each copy, replacement, deletion, skip and conflict with its reason,
  groups a copy and a deletion of the same contents as a *Rename* and applies it by moving the
  file in the destination (through a temporary name the journal records, so even a crash part
  way is undone), also for a rename only in letter case or accents on file systems that ignore
  them; warns when the filter leaves
  items out, and exports the plan as versioned JSON with relative paths, the filter and a
  fingerprint of the folder paths, so it is applied only to those folders. Applying asks for confirmation (twice for large deletions), checks every file against
  its SHA-256 fingerprint first, so a file edited after the preview is never overwritten, copies
  through a verified temporary file, shows the item being changed, and keeps replaced and
  deleted files in a recovery journal outside both folders. A failure or *Stop* undoes the changes; a synchronization cut short is
  offered for undoing at the next start. Symbolic links, file/folder clashes, folders renamed
  only in case and other names that differ only in case or in how accents on Latin letters are
  written are left alone, and folders
  inside each other are refused.
  On the command line: `--sync-plan mirror|update`, `--sync-apply <PLAN> --yes` and
  `--sync-recover`, with exit codes 4 (undone) and 5 (recovery required). A plan whose files or
  journal copies do not fit in the free space of their drives is refused before anything changes,
  and the JSON result has a `problem` (`stale`, `unsupported`, `read_failure`, `write_failure`,
  `canceled`, `recovery_needed`) when it is not a plain success.
- **Jobs:** `--job <FILE>` runs the compares, reports, sync plans and syncs listed in a versioned
  JSON job file (`.oxjob`) in order, each as its command line would, and prints a JSON summary
  whose exit code is the worst of the jobs. Paths are relative to the job file, `${env:NAME}` in
  paths is the only expansion, and no shell or other program is run. A sync job needs `"apply": true` in
  the file and `--yes`, and a large deletion also `"allow_large_deletion": true`. The jobs after a
  failure are skipped unless `"continue_on_error": true`; unknown fields and newer versions are
  refused.
- **Copy values in the semantic view:** *Copy value to left* and *Copy value to right* on each
  JSON, XML, TOML or YAML change replace, add or remove that one value on the other side. Only the
  value's text changes; comments and layout stay. The edited file is parsed again and the copy is
  kept only when it has exactly the expected structure; otherwise nothing changes and the view says
  why. Moves are not copied. Each copy is one undo step.
- **Three-way semantic comparison:** in a three-way compare, *Semantic* lists each path that the
  left or right file changed from the middle one as *Only left differs*, *Only right differs*,
  *Both differ the same way* or *Conflict*, with a filter for each. *Use left* and *Use right*
  copy that side's value into the middle pane. An array that both sides changed, where elements
  pair ambiguously, is one conflict for the whole array. `--semantic left base right` writes the
  same report, with `state` per path.
- **Structured files in folder compares:** *Treat JSON, XML, TOML and YAML files with the same
  structure as identical* in *Options → Folder* (or `--structured`) counts such files as identical
  in the *Full contents* method when only key order, layout or comments differ.

### Changed

- The README shows the OxideMerge icon next to its title.
- The English and Traditional Chinese READMEs are reorganized for users and use simpler wording.
  The build, test, continuous integration and release instructions moved to a new development
  guide (`docs/development.md`, `docs/development.zh-TW.md`).
- **Semantic TOML and YAML:** numbers compare by value also between integers and decimals (`1`
  equals `1.0`, `1e23` equals `100000000000000000000000`) and exactly, not as 64-bit floats; YAML
  integers too large for 128 bits compare by their digits, and a TOML integer outside 64 bits is
  reported as out of range, as the TOML specification requires. TOML date-times with a time zone
  compare by the moment they name, and YAML keys by value
  (`0x10:` and `16:` are one key, so a mapping with both `0x10:` and `"16":` is refused). An empty
  YAML document is `null`; `!!int`, `!!float`, `!!bool` and `!!null` must fit their value; tags on keys
  other than `!!str`, and on mappings or sequences other than `!!map` and `!!seq`, are refused.
  TOML files are limited to 4 MiB like YAML.
- **Release baselines:** a suspected rename with edits needs at least three shared lines, so two
  tiny files with one line in common are no longer paired. An important path also flags a move
  or rename from it. `--split-rename` and `--important-path` accept `\` between folders and a
  leading `./`; `--split-rename` reports a path that is not an added file, and both are refused
  outside baseline, release report and impact analysis.
- **Impact analysis:** Objective-C `#import "…"`, TypeScript `import x = require(…)` and `.js`
  imports of `.ts` sources are exact imports; a `.h` header that only reads as C++ is read as C++;
  only the `<script>` blocks of Vue and Svelte components are read; Rust attributes such as
  `#[derive(…)]` count as part of a definition, and a name defined twice changes with either
  definition. Several reasons for one line are one finding (rule `literal-reference-v2`, so
  name and path findings dismissed before need dismissing again), and lines with an exact finding are
  dropped before the 500 name and key findings are counted. Files of up to 1 MiB are indexed in
  parallel.
- Every interface language now translates the *Text layout* choices (*Aligned rows*, *Compact
  rows*).
- **User guides:** reorganized by task (text, merging, folders, binary, images, web pages, filters
  and rules, saving and reports, analysis tools, then the reference chapters), with the long
  paragraphs split into steps, lists and tables. The English guide uses short sentences in plain
  English for readers whose first language is not English; the Traditional Chinese guide uses
  Taiwanese terms and the program's own wording, without mixed-in English. The guides now
  describe the *Important path fragments* field of the baseline panel. All screenshots were
  taken again from the current version, and new ones show the *Select files or folders* dialog,
  the file compare below the folder list, the inline view, the CSV table view, the Formatted web
  mode, merge verification, semantic comparison, the review workspace and the release summary.
- **Web compare no longer uses a browser:** the Full page and Screenshot (visible area) modes are
  removed, with the Edge, Chrome, Chromium or Brave they started headless and the *Browser used to
  render pages*, *Screenshot width* and *Screenshot height* options (the options tab is now
  *Image / Binary*). The Formatted, Text, HTML source and Resource tree modes convert the HTML as
  downloaded, in the window and with `--cli`, so `samples/web/left.html` against `right.html`
  opens in a few milliseconds instead of about 2 seconds. Page scripts never run, so content they
  add is not compared. Settings saved with a screenshot mode open web pages in Formatted. The Text
  mode leaves out hidden elements, as the Formatted mode does, and no longer includes the page
  title. The *Render pages in the browser for text and HTML* option is removed.
- **Formatted web compare is faster:** a large table lays out only the rows in view, and measures
  its column widths once, so scrolling a page with a table of 1,500 rows takes about 2 ms a frame
  instead of 35 ms. The pages are converted, lined up and compared on the loading thread, so
  opening a large page no longer holds up the window.

### Fixed

- **Translations:** the message about skipped web resources now gives the right reason in every
  language, and *Saved as* no longer reads as a future action. English counts read correctly
  for one item (`1 difference(s)`). German, French and Russian messages read correctly with the
  side names filled in, and Russian counts agree in number. Terms are more consistent within
  each language (for example *tab*, *merge*, *fold* and *only on one side*), and Simplified
  Chinese uses *访达* for Finder. Every kind of outcome a comparison or a synchronization can
  report — *another reading is as likely*, *no longer as it was*, *not supported*, *could not be
  read*, *could not be written*, *stopped* and *needs putting back* — now has one name in each
  language, used wherever a feature leads with the kind of problem before its detail.
- **Web page compare:** *Stop* also ends the download of the pages and their resources as
  data arrives, instead of letting it run on in the background; a server that sends nothing is
  still waited for until its 30-second timeout.
- **Impact analysis:** a style sheet ending inside an `@import` no longer stops the analysis;
  files with lone carriage-return line endings report the right lines; cancelling stops the
  symbol phase too; the index cache no longer mixes up files read by different grammars; the
  first use of an imported name is no longer a property or key of the same name.
- **Semantic TOML:** changes in a table point at the table's own lines.
- **Semantic paths inside paired arrays:** a change inside an array paired by key or reordered
  now names the element on each side; before, nested paths and the old place of a move used the
  second side's position. An XML attribute change points at the attribute, not the whole tag.
- **File compare below the folder list:** *Next difference* and *Previous difference*
  (`Alt+↓`/`Alt+↑`, the *Compare* menu) now work in it, and past the last or first
  difference, they and its arrow buttons move on to the next or previous file with differences;
  *First difference* and *Last difference* (`Alt+Home`/`Alt+End`) work there too. A key pressed
  while a file is still loading waits for it. Without the docked compare, they select the next,
  previous, first or last file with differences, and with a review filter only the files it lists.
- The *Diff algorithm* option shows *LCS* instead of *Lcs*, and `--help` lists TOML and YAML for
  `--semantic`.
- **Merge verification** reports a merged preview that no longer parses as a semantic document.
- **Folder merge undo:** the copies are kept next to the merged files, so a deleted file is only
  moved, and moved back on undo, and no room is needed on another disk; files are put back
  atomically, a new `.bak` is removed only after its file was put back, removed folders come back
  with their permissions, and when a file cannot be put back the copies folder is kept, named in
  the error, and lists in `index.txt` which file each copy was. Each file is
  checked again just before it is written.
- **Review workspace:** saving waits until the decisions were checked against the files, and a
  decision on files that changed is saved stale instead of renewed. A file that cannot be read
  for a moment no longer passes its decision to another file with the same content.
- **Saving on Windows** no longer fails while a background re-read of the file is running, and
  retries briefly when another program, such as a virus scanner, has the file open.
- **Formatted web pages:** a list item, definition or block element now ends a hidden paragraph
  as in a browser; a stray end tag no longer ends a hidden element early; the `<base href>` in
  the page head resolves links; very deep hidden markup no longer slows the conversion.
- The *Array matching key* field stays in place and keeps its focus while the report is made.
- Folder column headers tell screen readers the sort direction again.
- *Review* links from impact findings do nothing for a file that is not in the comparison, instead
  of selecting a missing file.
- **Windows:** folder synchronization copies into a folder whose path is just under the old
  260-character limit. The name of the temporary copy counts towards the limit as much as the
  folder does, so a folder of 224 to 259 characters was still refused with *The system cannot
  find the path specified*.
- **Formatted web pages:** the difference marks beside the pages stay where they are while the
  pages are scrolled. A row is measured only once it is drawn, so scrolling through a page of
  tables replaced each estimate with a real height and moved everything below it, taking the
  marks and the scroll thumb with it. The overview and the scroll bar now use a height worked
  out from each row's content, which drawing never changes; the pages themselves still scroll
  by their measured heights, and the viewport box is carried between the two.
- **HTML:** comparing pages as *HTML source* no longer offers the Semantic and Tree views. HTML
  is not XML, and a page that opens with an XML declaration, as an XHTML page does, was taken
  for an XML document; the views then had nothing useful to show.
- **Folder synchronization:** a synchronization that has just succeeded is no longer offered for
  undoing. A window looking for synchronizations that did not finish read each journal's header
  before taking its lock, so one that finished in between — removing its header and only then
  letting the lock go — was still listed, and undoing it would have taken back correct work. The
  header is now read again once the lock is held, and *Undo those changes* takes the lock before
  the header for the same reason. A journal that has gone by then is nothing to undo rather than
  a failure.
- **Windows:** folder synchronization copies and deletes files whose path is longer than the old
  260-character limit. Such a path was staged correctly but could not be renamed into place, so
  the file was refused with *The system cannot find the path specified* and the changes before it
  were undone. Paths are now handed to the system in their extended form where they are too long
  for the old one.
- **Folder synchronization:** a file another program holds open is reported as one that cannot be
  read, rather than as one that changed after the preview. Nothing is touched either way, but the
  advice the message leads to is different. Where several things are wrong at once, the kind of
  problem reported is the one named by the first message rather than the last.
- **Tree view:** `Tab` reaches the tree, which is one stop in the tab order as a tree should be:
  it draws a focus outline, the arrow keys then move through the rows without clicking a row
  first, and what holds the keyboard is named after the current row, so a screen reader reads out
  the row's state and both of its sides. Previously the tab order went past the rows and the
  arrow keys only worked after a click.

## [0.9.7] - 2026-11-01

### Added

- **Inline view** for 2-way text comparisons (the toolbar toggle next to word wrap, or *Inline
  view* in the command palette): one column with removed lines above the added lines that replace
  them, old and new line numbers, and unchanged runs folded. Moving between differences and copying
  them to the other side work as in the side-by-side view; the arrow keys move along the lines and
  Copy copies the current line. Editing and selecting lines stay in the side-by-side view.
- **Merge bar** for 3-way comparisons: how many conflicts are left, *Next conflict* (wrapping
  around), *Take left*, *Take both*, *Take right*, *Mark resolved* for a conflict edited by hand,
  and *Save result*, which turns into the main button once no conflicts are left.
- **Start page launcher:** type, paste or browse two (or, with *3-way*, three) paths and compare
  them without opening a dialog; *More options…* opens the full Open dialog. Recent comparisons
  are cards. The first start offers a sample comparison in a temporary folder, which is removed
  when its tab closes and is not added to the recent list.
- **Accessibility options** in *Options → General* and the command palette: color-blind friendly
  diff colors (blue, orange, purple and yellow instead of green and red), high contrast, reduce
  motion and compact density.
- **Search commands** field (⌘K / Ctrl+K) in the title bar, which opens the command palette;
  narrow windows keep the icon.
- **Formatted web page compare:** the new *Formatted* mode, the default for web pages and HTML
  files (a mode chosen before is still remembered), turns each page into Markdown and shows it
  formatted, side by side, with headings, emphasis, links, lists, quotes, code blocks and
  tables. Pages are compared block by block; changed blocks are highlighted and lined up with
  the changed block of the same kind on the other pages (a heading with one of the same level
  first), with the changed words highlighted within them as in the text compare (following
  *Inline differences*). Tables are drawn with lines around their cells; their rows are lined
  up, with the changed words in each cell highlighted, and cells of rows or columns only one page
  has highlighted whole. The difference buttons and keys step through them, and as in the text
  compare the current difference is highlighted more strongly and the blocks being compared are
  outlined on every page; clicking a block makes it the compared one, and stepping goes on from
  there. The pages scroll only when the difference is out of view, and a location pane gives an
  overview of the differences, as in the text compare. Content the browser
  does not show, also when a style sheet hides it, is left out, and images appear as links.
  Text that reads like a character reference, such as `&copy;` on a page about HTML, is shown
  as written. The
  console compare (`--cli -t web`) compares the Markdown when this mode is selected. Sample pages
  with complex text, headings and tables are in `samples/web/formatted`.
- **HTML files open in the web page compare** (`.html` and `.htm` on every side), so they show
  formatted by default; `-t text` or *Compare as → Text* compares their source, and a merge
  with an output file (`-o`) or a console `--patch` still uses the text compare.
- **TOML and YAML semantic compare:** the *Semantic* view and `--semantic` also read TOML and
  YAML (`--format toml` or `yaml`, or from the extension), comparing tables and mappings without
  regard to order and numbers by value. YAML is read by the YAML 1.2 core schema, one document of
  at most 4 MiB without aliases, complex keys or custom tags, within the existing nesting and
  node limits.
- **Semantic changes as a path tree,** with a count on each branch, and a **Moved** kind for array
  elements that only changed place. *Array matching key* (`--array-key MEMBER`) pairs the object
  elements of arrays by a member such as `id` instead of by position.
- **Release summary:** the time each side was captured, category counts that filter the list,
  and a *High priority* list of changes with risk flags. A deleted and an added text file that
  share at least half of their lines are shown as a **suspected rename** with the share of
  matching lines; *Not a rename* (`--split-rename PATH`) keeps them apart.
- **Exact impact findings:** *Potential impact* recognizes JavaScript/TypeScript, Python, C/C++,
  CSS, Rust and JSON `$ref` imports of a changed file and labels them exact; changed nested JSON,
  TOML and YAML keys are also searched for as dotted paths. In a folder comparison, *Review* opens
  the file of a finding in the review workspace.
- **Merge plans warn** when both sides edit lines close together or change the same setting in
  different places.
- Review decisions follow files renamed without changes, and ambiguous renames are marked for
  review again; review bundles with unsafe or missing entries are refused.
- **Function and type impact:** for changed JavaScript/TypeScript, Python, C/C++ and Rust files,
  *Potential impact* compares the top-level functions, types and constants, and a file importing a
  changed or removed one by name (also under another name) gets an exact finding at its first use;
  the name's first use in other files of the same language is a heuristic finding. Matches of
  names and configuration keys stop at 500 with a note counting the rest, instead of failing the
  analysis when a broad change hits common words.
- **How sure a semantic change is:** each change says whether its array elements were paired by
  the array matching key, as a reordering or by position, and changes another reading explains as
  well, such as either element of a swap, are marked *ambiguous*. A note says when the array
  matching key cannot pair an array. Reports add `confidence`, `ambiguous` and `notes`.
- TOML and YAML samples: `config/service.toml` and `config/deploy.yaml` in `samples/left` and
  `samples/right` differ in layout almost everywhere but in only a few settings, and
  `samples/three/settings` holds the 3-way merge settings as TOML and YAML as well as INI.
- Saved JSON and XML semantic, release summary and impact reports in `tests/fixtures/reports`;
  the tests fail when a report of the same inputs differs from them.

### Changed

- **Folder auto merge is all or nothing:** every item is checked before anything is written, and
  if one changed since the plan nothing is merged. A *Stop* button ends a running merge, and
  stopping it or a failed change (a read-only folder, a full disk) undoes the changes made so far.
- *Potential impact* reads imports and definitions of JavaScript/TypeScript, Python, C/C++ and Rust
  files from their syntax trees, parsed with Tree-sitter, and imports of other files from the
  code's tokens, so imports spanning lines are found and imports inside comments and strings are
  not; Rust `use crate::…`, `super::…` and `self::…` paths and JavaScript re-exports count as
  imports. A file with code the parser could not read is listed as partly assessed. The
  application is about 10 MB larger for the grammars.

- The footer of HTML reports of folder and file comparisons links *OxideMerge* to the project
  page, https://github.com/kuopinghsu/OxideMerge.
- CI builds, tests and packages the macOS app, and runs the macOS benchmarks, on the macOS 15
  runner image, since the macOS 14 image is retired on November 2, 2026. The app still runs on
  macOS 11 and later.
- Review, verification and baseline panels open in a side drawer, toggled by the button at the
  right end of the toolbar, instead of taking space below the comparison.
- **Folder comparison:** chips under the paths count *All*, *Different*, *Only in Left/Right* and
  *Identical* items and show or hide each kind with a click, replacing the counts in the status
  bar. Folder rows say how many items differ inside them; a colored bar at the left edge marks
  differences instead of tinting whole rows; one *Modified* column shows the newest date and which
  side is newer; the *Folder* column is left out in tree mode. In a 3-way comparison, an item
  missing from one side only is counted, shown and hidden with *Different* rather than with
  *Only in*, which now covers items on one side alone. Folders left uncompared by a comparison
  that is not recursive count only in *All*, as in the console comparison, and stay listed
  whichever kinds are hidden.
- The text comparison toolbar is grouped into Differences, Merge, Edit and View, with captions in
  wide windows. The status bar no longer repeats the difference count shown in the toolbar.
- The gutter between panes is calmer: the connector of the current difference stands out, the
  others are faded, and the copy arrows appear on the current or hovered difference.
- Tab titles name a file shared by both sides once, with the folders that differ as a subtitle.
  Tabs with unsaved changes show a dot in place of the close button until hovered.
- The user interface is built on egui and eframe 0.37 (from 0.31), which the formatted web page
  view needs. Menu items sit slightly inside their menu, and screen readers name menu items by
  their label alone, without the shortcut, and drop-downs by the caption beside them.
- Icons are drawn instead of taken from font symbols (sort arrows, tree arrows, warnings and the
  Help icon), so they look the same with every font, and colors, sizes, corners and spacing come
  from one set of theme values. The common icons (compare, folder, file, search, settings, theme,
  arrows, history, filter, export, image, binary, web page, add, check and side drawer) follow one
  outline set on a 24-unit grid, and all icons use its lighter line; *Options* now shows two
  sliders instead of a gear. Buttons, chips and toggles show distinct hover, pressed, selected,
  disabled and focus states in every theme and high contrast mode, and dialog buttons react to
  hover.
- Toolbars start a new row for a group that no longer fits instead of running past the window
  edge, and folder columns start narrower in narrow windows so every column stays in view.
  Narrow windows leave out the name next to the logo in the title bar.
- Screen readers name the tree arrows ("Expand …" / "Collapse …"), the Git revision picker, the
  scroll bars, the location pane and the comparison areas.

### Fixed

- The Formatted web page compare stops at every changed block when stepping through the
  differences. Changed blocks next to each other counted as one difference, so *Next difference*
  skipped all but the first, such as an item added after a changed one at the end of a list.
- Decisions on documents compared through a converter stay current when a review is reopened,
  instead of always being marked for review again.
- Merge plans no longer warn that an insertion on one side is a deletion.
- *Next unreviewed* and the links to a file in the review workspace open folders collapsed in the
  tree to show the file.
- The close button of the Help & Shortcuts dialog sits at the right edge of its title bar, as in
  the About dialog, instead of short of it: the dialog is now exactly as wide as its panels and
  scroll bar, and shortcuts too long for their column, such as `Shift+↑ ↓ / Shift+PgUp PgDn` on
  Windows and Linux, wrap instead of widening a panel.
- The close button of the About and Help & Shortcuts dialogs is announced as "Close" to screen
  readers.
- **Web page compare** no longer leaves out text inside `display: contents` elements, which
  hid it from the Text mode and the highlights altogether, nor the text of web components
  (open shadow roots, with their slotted content in place). Highlights are painted in shadow
  roots too.
- With *Render pages in the browser for text and HTML* off, the Text and HTML source modes show
  the pages as downloaded even after a screenshot mode has rendered them. Rendered pages are
  rendered again after the browser or the screenshot size changes in Options.
- **Web page compare** is sturdier with pages that misbehave:
  - A page that redirects after it loads, or opens an `alert`, `confirm`, `prompt` or print
    dialog, no longer fails the comparison or waits until the time limit: dialogs are dismissed
    and the page the redirect leads to is read.
  - A page that cannot be reached, does not load in time or crashes its tab is reported in its
    own pane header with a blank screenshot, and the other pages are still shown. Error pages
    with an empty body (such as an empty 404) show their status instead of failing.
  - The pages are asked for at the same time instead of one after the other, so slow pages no
    longer add up, and Stop or closing the tab ends the wait at once instead of after the time
    limit.
  - Text and highlights are read in a separate script context, so pages that redefine
    `Array`, `JSON` or other globals cannot break them, and a page whose highlights cannot be
    painted is still compared.
  - XHTML pages and text with unpaired surrogates are read correctly, and text hidden inside a
    closed `<details>` or `content-visibility: hidden` element is left out.
  - On macOS and Linux the browser no longer asks for keychain or password store access, its
    messages are limited in size, and its pipes are not passed on to other programs.
- The folder comparison toolbar no longer draws its filter, rules and method chips over *All
  differences* and *Export HTML* when they do not fit, and the image comparison toolbar no longer
  runs past the window edge in windows narrower than about 1200 points: both switch to icons and
  shorter chips. Title bar icons that no longer fit are left out rather than overlapping the
  menus.
- In 3-way text comparisons, windows narrower than about 780 points no longer push the panes, the
  pane footers and the scroll bar past the right edge.
- Very short panes no longer crash the custom scroll bars.
- In dark high contrast mode, hovered accent buttons are told apart from resting ones.

## [0.9.6] - 2026-10-30

### Added

- About and `--version` show a short Git revision for development builds, with `-dirty` for
  tracked source/index changes; clean optimized builds at the matching version tag retain the
  UTC build date. The copyright year always uses the calendar date.

- Escape closes the active comparison tab after open dialogs, search, previews and editors
  have handled it, retaining the existing unsaved-changes confirmation.

- Configurable compact/aligned text rows, including blank insertion/deletion spacing in aligned
  mode; text and binary location panes consistently appear on the right.

- Opt-in merge plans and saved-result byte verification using the existing merge engines, with
  explicit conflict, mechanical-check and heuristic-warning states; `--merge-plan` and
  `--validate-merge` provide versioned analysis reports without writing comparison inputs.
- Read-only JSON/XML semantic views and `--semantic`, with changed paths, decoded source spans,
  duplicate-key/entity/depth guards and textual fallback.
- Persistent file/hunk review decisions, comments, rationale, tags, stale-source detection and
  summaries in additive `.oxmreview` version 2 manifests; version 1 remains readable.
- Frozen folder/Git/snapshot baseline summaries, unique exact-content move detection, optional
  important-path flags and evidence-only potential-impact reports. All reference findings are
  heuristic and do not certify builds, tests or compatibility.

- **Document conversion throughout comparisons:** full-content folder scans, individual files,
  command-line comparisons, docked previews and folder reports share the same external converters.
  Options include presets for PDF text (pdftotext), Office/OpenDocument (Pandoc), and Apache Tika,
  with dependency hints. Converters are limited to 30 seconds and 16 MiB per output stream;
  stopping a folder scan cancels its running conversions. Failed conversions report errors even
  when they produced partial text. Extracted documents remain read-only.
- **CSV/TSV record editing:** table view handles quoted multiline records and escaped quotes,
  offers resizable columns, and lets you edit or copy individual cells through the context menu.
  Cell changes preserve untouched field quoting, encoding and line endings, with undo/redo.
- **Portable folder reviews:** save a folder comparison as `.oxmreview`, or right-click to save
  only selected files/folders. Bundles include captured contents, effective comparison settings,
  and folder bookmarks/comments, and reopen as read-only snapshots without the original files.
  Converted documents are captured as text; converter commands are not included or run on reopen.
  Folder comments are included in HTML, XML and text reports.

### Fixed

- Merge verification includes encoding/BOM/EOL conflicts and rejects matching base bytes while
  a format conflict remains unresolved; independent text/format changes remain mergeable.
- XML semantic parsing tracks child positions per parent, avoiding quadratic sibling scans on
  wide documents while preserving attribute, comment, mixed-content and reference semantics.

- Large UTF-8 text comparisons load incrementally, borrow unmodified comparison keys and use
  compact unique-line anchors with coalesced equal runs and in-place reconstruction. Alignment
  rows shrink from 64 to 16 bytes; pane display maps share rows. Release comparison dictionaries
  before alignment and avoid whole-document syntax-worker copies beyond 50,000 lines or 16 MiB.

- Folder merge confirmation rechecks SHA-256 content hashes so same-size edits with restored
  modification times are detected before overwriting a changed file.

- Merging a CSV/TSV difference preserves other changes in the same multiline record; partial
  merges that would break quoting are rejected without modifying the document.
- Table view supports record and column keyboard navigation, F2/Enter cell editing, clipboard
  copy/paste and Go to Line. Clicking a multiline record selects a difference within that record.
- Portable folder reviews retain captured line filters, substitutions and ignored blocks from
  the initial scan onward, including after refreshing on another machine.

## [0.9.5] - 2026-10-29

### Added

- **Synchronization points** (*Merge → Set synchronization point*, Alt+S, or right-click a
  line): line up lines the compare pairs wrongly, as in WinMerge. The text before and after each
  point is compared separately. Points are marked in the gutter, follow edits and are kept in
  saved comparisons.
- **XML reports** of text and folder compares, for other programs to read: choose the `.xml`
  type in *Generate report...*, or `--report <name>.xml`. Text reports list every difference
  with its lines and the bookmarks; folder reports list every item with its status, dates and
  sizes. The layout is described in the user guide.
- **HTML slideshow reports** (*File → Export → Generate slideshow report...*, or `--report
  <name>.html --slideshow`): one difference per slide, with a title slide, keyboard navigation,
  full screen, a link for each slide and one slide per printed page, as Araxis Merge makes them.
  Folder slideshows cover every changed text file.
- **Web page compare, full page:** a new *Full page* mode captures each whole page, top to bottom,
  instead of only the first screen (up to 16384 pixels tall).
- **Web page compare, highlighted differences:** the screenshot modes compare the pages' text and
  highlight what differs on the pages themselves (changed blocks in yellow, changed words in
  orange), like WinMerge's *View Differences*, so a change that moves the page down no longer
  marks everything below it. *Highlight differences* in the toolbar turns this off.

### Changed

- **Web page compare** drives the browser over the DevTools protocol, with all pages loading at
  the same time in one browser. Text and HTML source now come from the page as the browser shows
  it, after its scripts have run, when a browser is found (*Render pages in the browser for text
  and HTML* in the options). Text puts each heading, paragraph, list item, table cell and form
  value on its own line and leaves out hidden elements. Text is the default mode.
- **Menus:** every pull-down menu opens equally wide: the menu bar's menus, their submenus, the
  right-click menus and the folder compare's drop-downs (the widest menu of the menu bar, its
  submenus included, sets the width, at least 220 points). Options that turn on or off and choices
  of one show a check mark in a column of their own, so labels line up and an option that is off
  no longer looks disabled. Shortcuts keep clear of their labels. The tab's right-click menu shows
  the Close shortcut, and a file's right-click *Compare as* offers Web page like the Compare menu.
- **Web page compare:** when the browser fails, Text and HTML source fall back to the pages as
  downloaded and the status bar says why, instead of showing only an error. The console compare
  (`--cli -t web`) also renders the pages in the browser, so it matches the app. On macOS and
  Linux the browser is driven over a private pipe instead of a local port. Tests that need a real
  browser run only when `OXIDEMERGE_TEST_BROWSER` is set; CI sets it on the Windows and Linux
  x64 runners.
- **Release files:** a release now holds nine files: a `.zip` per Windows architecture holding
  only `oxidemerge.exe`, an AppImage and a `.tar.gz` per Linux architecture, the macOS `.dmg`,
  and the two PDF user guides. The separate Windows `.exe` downloads, the Linux `.deb`
  packages, the macOS `.zip` and `SHA256SUMS.txt` are no longer published; GitHub lists the
  SHA-256 digest of each file on the release page. The release fails if any file is missing or
  an unexpected one is present.

### Fixed

- **UTF-8 patches:** inserting or deleting lines at the start of a file with a byte order
  mark now keeps exactly one mark at the file's start, including when ignore rules match
  the line that moves across that boundary.

- **Windows CI:** the Git revision CLI test explicitly checks LF and CRLF checkout modes
  instead of assuming LF output, and isolates the subprocess Git configuration from the runner.

- **Mixed line endings:** automatic merge combines independent terminator edits on aligned
  lines, including around insertions and deletions, and keeps the merged endings on save.
- **Patch ignore rules:** anchored filters, substitutions and ignored blocks are evaluated
  against decoded text so BOMs and CRLF bytes cannot change which edits are exported.

- **Text formats and merging:** comparisons account for encodings, BOMs, line endings and
  final newlines. Copy All transfers selected formats across multiple differences, independent
  text and format edits merge together, and saving refreshes format differences.
- **Patches:** UTF-8 patches preserve BOMs, LF/CRLF endings and missing final newlines, including
  changes that move the last line. Unsupported encodings and lone-CR endings report an error.
- **Projects:** saved projects retain pane descriptions and read-only flags and use relative
  input paths where possible. The guides clarify save-shortcut scope, and the security document
  explains trusted Git filters and their possible command execution and network access.

- **Four or more paths:** opening four or more files or folders, from the command line, a drop
  or a file manager, no longer crashes the folder compare; it says that two or three paths are
  needed.
- **File safety:** saving and copying now report an error if a temporary file cannot be created
  or cannot replace the destination. They no longer fall back to overwriting the original file
  directly, which could truncate it if a subsequent write failed.

## [0.9.4] - 2026-10-27

### Added

- **Tests:** randomized diff, 3-way merge and patch tests that check every result against the
  inputs (`OXIDEMERGE_TEST_CASES` and `OXIDEMERGE_TEST_SEED` for longer runs or to repeat one),
  tests that draw the views without a window, and command line tests of the built program
  (`tests/cli.rs`). `OXIDEMERGE_CONFIG_DIR` points the program at another settings folder, window
  size and place included, so tests never touch the real one.
- **Window smoke test** (`make gui-smoke`, `scripts/gui-smoke.sh`): opens real windows with the
  release build, two files, three files, two folders, and checks what each tab shows. CI runs it on
  Windows (with Mesa's software renderer), Linux (Xvfb) and macOS and keeps the output of a failed
  test.
- **Benchmarks** of text, 3-way, binary, image and folder compares, timed with their peak memory
  (`OXIDEMERGE_BENCH_*`); `docs/benchmarks.md` has the method and a first baseline, and a CI run
  started by hand can run them on every platform.
- **Dependency checks** with `cargo deny` (`deny.toml`) for known vulnerabilities, licenses and
  sources, in CI and before every release, and `SECURITY.md` on reporting vulnerabilities and on
  which inputs OxideMerge treats as untrusted.
- **CI checks the packages:** the Windows `.zip` must hold the program, README and license; the
  Linux `.deb` is installed, run and removed, leaving the user's settings alone; the macOS app must
  hold both architectures. Before publishing, the release job checks that every package is there
  and that the version was not released before, and the release notes name the commit built.
- **Documentation:** a Quick start in the READMEs, a Compare types section and a longer
  Troubleshooting and limitations chapter in the user guides, and `docs/macos-signing.md` on
  Gatekeeper and notarization.

### Changed

- **Folder auto merge** checks each item again just before it changes it. Files are replaced
  through a temporary file, so a crash or a full disk leaves them whole, and are kept as `.bak`
  with backups on. Nothing is changed when the folders changed since they were compared. A file
  and a folder of the same name, symbolic links, special files and items the compare could not
  read are conflicts left alone; links are never followed, and names that would lead outside the
  middle folder are refused. A step that fails is reported and the others still run. Copies in
  the folder compare are made through a temporary file too, and keep the file's modification
  time.
- Reports, patches and saved comparisons are written through a temporary file, so a failed export
  never leaves half a file behind. Devices such as `/dev/stdout` are written directly.
- **Web page compare:** a web page may not load local files, and only a page on this computer or a
  private network may load resources from one or be redirected to one; such resources are skipped
  and counted. A page counts as local only when every address of its host is; IPv4 addresses
  inside IPv6 ones (mapped, NAT64, 6to4) are judged by the address they hold. Redirects are
  followed by OxideMerge itself, at most 10; a redirect without a target is shown as it is, and
  one to anything but a web address says so. A byte order mark now
  decides the encoding before the HTTP header and the `<meta>` charset.
- Language files in a `Languages` folder are checked: messages with other placeholders than the
  English ones, unknown keys and non-text values are left out, and English is shown instead.
- The menus of the menu bar all open as wide as the widest one, in every language.
- macOS instructions describe **Open Anyway** in System Settings, as Control-click → Open no longer
  bypasses Gatekeeper from macOS 15 on.

### Fixed

- Applying a patch made without context lines (`diff -U0`) put added lines one line too early,
  and in the wrong place when hunks before them were left out of the patch. A hunk found away
  from its stated line now moves the hunks after it by as much, as GNU patch does, so a line
  added after it no longer lands too early.
- A difference next to a moved insertion or deletion could point at the wrong line of the side
  where it is empty.
- A folder compare that broke off with an internal error showed the folders as identical; it now
  says it failed and counts as an error.
- A text compare whose syntax highlighting stopped with an error kept redrawing the window.
- Settings that could not be saved were dropped without a word; the error is now shown once,
  and saving is tried again until it works and when the program closes.
- Settings that could not be read were replaced by the defaults when settings were next saved.
  The file is now kept as `config.json.bad` (or `oxidemerge.json.bad`) and a message says so; a
  settings file that can't be opened at all is not saved over. An empty file still holds the
  defaults, as for portable use.
- With the Recycle Bin option on, a file or folder that could not be moved to the trash was
  deleted for good without a word; it is now left where it is and the error is shown.
- Folder auto merge could miss a file that changed while the merge was being planned.
- On Windows, copying a file over a read-only one in the folder compare or folder auto merge
  wrote into it in place; it is now replaced through a temporary file and stays whole if the
  copy fails.
- The Filters window opened folders with `xdg-open` on macOS; errors opening files and folders
  from the folder compare and the dialogs are now shown.
- On Windows, *Show in file manager* did not select files whose path has a space or a comma.
- Changing a file's line ending in a text compare could not be undone, and undoing an earlier
  change cleared the mark that the file was modified, so closing it lost the new line ending.
- Folder auto merge merged binary files as text, and dropped a change of encoding, byte order
  mark, line endings or final newline made on one side. Only text files that read back to the
  same bytes are merged now, the change of either side is kept, and different changes on both
  sides are a conflict. A side left without line breaks no longer counts as changing the line
  ending, and the encoding and byte order mark are merged together, so UTF-16 never loses its
  byte order mark.
- Folder auto merge with backups on could replace a compared file with another file's `.bak`;
  such a step is now left alone. A folder added on both sides is now made in the middle folder,
  even when it is empty, and its contents are merged item by item.
- A read-only `.bak` left from before stopped the next backup of that file. On Linux and macOS,
  a read-only file written in place is made writable for its owner only, and read-only again
  when the write fails.
- Files kept under `/dev/`, such as `/dev/shm`, were written in place and without a backup, like
  devices.
- Resources of a web page saved under one name were numbered `a~2`, `a~2~3`, ... until the name
  got too long; they are now `a~2`, `a~3`, ...
- On Windows, saving over a read-only file failed and left a temporary file next to it; the file
  is now replaced and stays read-only, as on Linux and macOS.
- Text copied into a new or empty file in a text compare got this computer's line endings; the
  file now takes the encoding and line endings of the file copied from, as in All differences.
  Take both and Auto merge into an empty middle file do the same, and undo puts back only what
  copying changed.

### Security

- Unpacker commands could run part of a file name as a command; the file path is now passed as an
  argument (`"$1"`) on Linux and macOS, and as an environment variable the command line quotes on
  Windows, where unpackers given the quoted path in the command line also failed to run. Quotes
  written around `${file}` are dropped, as they would leave the path unquoted.
- On Windows, *Open with* and opening a file from the folder compare went through `cmd start`, so
  a file name containing `&` could run a command; files are now opened by the shell directly.
- Resource tree downloads could write outside the chosen folder through an encoded `/` (`%2F`) in
  a resource address.
- The web size limit counted compressed bytes, so a small compressed response could unpack to
  fill memory; it now counts the bytes unpacked. A page of deeply nested `<head>` tags, or with a
  title tag left open in `<head>`, crashed the program.
- On Windows, a local page in a resource tree compare could load `file://host/share` resources,
  which opens a connection to that computer and hands it the user's credentials; files of other
  computers are now skipped.
- Archives that unpack to more than 8 GB or 500,000 files are refused, so a small archive can no
  longer fill the disk; `.tar.xz` archives are no longer unpacked into memory.
- Links in `.tar` archives that point outside the archive, directly or through other links, are
  no longer extracted.
- Web pages can no longer read local files or reach this computer and the private network
  through their resources or redirects (see Changed).

## [0.9.3] - 2026-10-26

### Added

- A user guide in AsciiDoc, in English (`docs/user-guide.adoc`) and Traditional Chinese
  (`docs/user-guide.zh-TW.adoc`), with screenshots of each view in both languages, linked from the
  READMEs. CI builds both as PDF files, attached to each GitHub release; `make user-guide` builds
  them locally.
- **All differences** (folder toolbar, Compare menu or command palette): every changed text file
  of a folder compare in one tab, with its differences side by side as in the HTML report, a file
  list with search, expand and collapse all, next and previous difference across files, copying a
  difference (or, with two sides, a whole file) to another side, undo and save (the folder compare
  then refreshes). The files are those the folder compare shows, its filters applied. Copying from
  a side where the file is missing deletes it when saved, with folders left empty (with backups
  on, the file is kept as `.bak`), a file changed on disk since it was read is never saved over
  and its card can discard the changes to read it again, text compares of saved files notice the
  change, opening the tab again lists files with differences found since, and the text of files
  far out of view is let go and read again when needed, keeping memory use down.
- **Numeric tolerance** (Options → Compare, or `--numeric-tolerance`): numbers at most a set amount
  apart count as equal, in text and folder compares.
- **Block filters** in compare rules files: ignore everything between a start line and an end line
  matching regular expressions.
- **Compare rules files**: lines to ignore, substitutions and blocks to ignore are kept in
  `.oxrules` files instead of the options. Options → Compare rules picks the default file and edits,
  creates or deletes them. A folder compare picks its own file from the toolbar, and Refresh (F5)
  reads it again. The context menu of a text compare adds a rule from the clicked or selected
  lines, and Compare → Compare rules switches the file of the active compare. Rules files are
  exported (one, or all to a folder, never replacing files already there) and imported in
  Options → Compare rules, or imported by dropping one `.oxrules` file on the window (or several on
  the options; two dropped on the window are compared). Renaming or deleting a rules file updates
  the compares using it, and restored session tabs keep their own file. A selected file that
  can't be read stays selected, with a message, and compares run without it. Projects and the
  `--rules` option name a file too (`--rules ""` for none). Rules set in the options of earlier versions move to a
  "Default" file.
- **Line detail pane** (View → Text compare): the current line of every side, one above the other and
  wrapped.
- **Edit markers** in the gutter for lines changed since the files were opened.
- **Statistics** (Tools menu): line counts per pair of sides for text compares, and file counts and
  sizes for folder compares.
- **Indent and unindent** the selected lines (`Ctrl+]` / `Ctrl+[`).
- **Bookmark comments**, shown when hovering the gutter, in the status bar and in HTML reports.
- **Save comparison** (File menu): every side's text with its bookmarks and comments in one
  `.oxmcmp` file, which opens again read-only.
- **Compare with a Git revision** (File menu, or `--git-rev`) for files and folders. The revision
  side is read-only and has the files as a checkout would, including Git LFS files and
  submodules.
- **File compare below the folder list** (View → Folder list): the selected file's differences without
  opening a tab.
- **Folder report with file differences** (File menu, or `--report-diffs`).
- **New HTML reports** for text and folder compares: one self-contained page listing every
  difference as a card with line numbers, context and highlighted words. Summary cards, an index of
  files and differences, side-by-side or unified view, light and dark themes, status filters,
  search, and `N` / `P` to jump between differences. Reports name the compare rules file used
  and count the differences it ignored. Folder reports with file differences list those files
  first, skip binary files and files over 16 MB, and cut lines after 2,000 characters; text
  reports stop after 20,000 rows and count the differences left out. Reports are made in the
  background. Printing opens every file and keeps the colors.
- **Export HTML** button on the toolbar of text and folder compares, also in the File menu and the
  command palette (`Ctrl+Shift+E`): saves the HTML report, with the differences of every changed
  file for a folder compare, under a name taken from the compared files (spaces written as `_`),
  and opens it in the browser (File → Export → Open reports in the browser after saving). The file
  compare docked below a folder compare exports its own report.
- **Copy to folder** and **select by name** in folder compares.
- **3-way folder auto merge** into the middle folder, merging text files changed on both sides when
  the changes don't overlap. Files hidden by the filter, and files changed after the merge was
  planned, are left alone.
- **Arm64 packages** for Windows (`.exe`, `.zip`) and Linux (AppImage, `.deb`, `.tar.gz`),
  built and tested on Arm machines, next to the x64 and macOS universal packages.

### Changed

- The sides of an archive compare are read-only: they are temporary copies, and changes to them
  were lost.
- **Menus regrouped** by what they act on. A new Compare menu holds stepping through
  differences, Compare as, Compare rules, the compare options and Refresh. File gains Open recent
  and Export (HTML report, reports, patch) submenus and takes Apply patch from Tools. Edit groups
  the bookmark commands in a submenu. View has the Folder list and Text compare submenus, Zoom,
  Theme, and next/previous tab. Commands that do nothing in the current tab are greyed out
  instead of hidden or silently ignored.
- Shortcuts in menus, the command palette and tooltips come from the same table as the keys
  themselves, and on macOS are written the macOS way (`⇧⌘S`, `⌥↓`, `⌘Q` for Exit) everywhere.
- Next and previous tab are `⌃Tab` and `⌃⇧Tab` on macOS, where `⌘Tab` switches applications.
- A key held with Alt no longer also runs the command of the key alone (`Alt+F4` no longer goes to
  the next bookmark first), and the command palette lists only what works in the current tab.

### Fixed

- `⌘Q` on macOS asks about unsaved changes like closing the window does, instead of quitting at
  once, also when the window is minimized or the app hidden and when quitting from the Dock, at
  logout or from a script.
- Saving text in a legacy encoding (Big5, Shift_JIS, ...) that can't hold some of its characters
  is refused with a message, instead of writing them as `&#...;` entities.
- A relative path given to `--rules` or in a project file is read from where it was given (the
  current folder, or the project's folder), and a missing rules file is reported.
- Rules files saved with a byte order mark are listed; a name ending in `.oxrules` is not doubled,
  names differing only in case (where file names keep case) are both listed, and names compare
  without case beyond ASCII. The rules editor shows the name a file will be saved under, takes
  `a =>` without a space after the arrow, marks lines with an empty side, refuses to save while
  lines are unclear, and asks before Cancel or closing the options throws edits away.
- Adding a rule to a rules file given by path that can't be read no longer writes another file.
- Close all, Close others and Close tabs to the right ask about each tab with unsaved changes in
  turn, not only the first; Esc cancels the close and exit prompts.
- New files are created with the usual permissions instead of readable only by their owner.
- Folder reports with file differences share one size limit across files, like text reports.
- Reports no longer name a compare rules file that could not be read, as the compare ran without
  it.
- Saving in All differences says which files had mixed line endings written as one kind, and a
  text compare stops showing "mixed" for a file once it is saved.
- A file with one difference says "1 difference" in All differences and in HTML reports.
- Three-way reports colour only the sides that changed. The CSV report starts with a byte order
  mark so spreadsheets read it as UTF-8, and keeps names like `-2+3+cmd|...` as text.
- Saving no longer empties a file when the disk is full or the disk quota is used up: a save that
  fails leaves the file as it was, and no empty file when it was new. The options are saved the
  same way.
- Compare options given on the command line (`--ignore-case`, `--lang`, ...) apply to that run
  only, instead of becoming the saved options the next time an option changed. Each one is kept
  only if it was changed in the program afterwards.
- Patches leave out the differences the compare options ignore, as `diff -B` does, except inside
  a hunk with other changes, where leaving them out would make the patch drop lines. The console
  compare of three binary files reads the third one, and its byte ranges end at the last byte that
  differs.
- Reports and patches of a folder compare wait until it has finished, and quitting waits for a
  report still being written. Report file names get `.html` when typed without it.
- Adding a rule to a typed name with a folder in it (`Team/Logs`) saves it in the rules folder
  (as `Team_Logs`) instead of a file without extension in the current folder. Importing a rules
  file named like one next to the program adds it under a free name instead of hiding that one,
  and a rules file dropped on the window updates the open compares. Renaming or deleting a rules
  file updates the compares using that very file, telling names apart by case where file names
  keep it. A project opened by a relative path reads its rules file from the project's folder.
  The Compare rules menus tick a rules file used by its path only when it is the one listed, not
  another file of the same name.
- The rules editor keeps the file of a rules file whose name is not a usual file name while the
  name is unchanged, renames it back when saving after a change of case fails, warns again about
  unsaved edits after further typing, and checks the patterns that will be saved.
- All differences reads files only as fast as the tab shows them and also lets go of their rows
  far out of view, keeping memory down; reads files changed on disk in the background, a few at a
  time; no longer replaces saved text with a reading made before the save; takes the line endings
  along with a whole-file copy; counts files saved rather than sides; says when files are still
  being compared, when an undo can't be applied, and drops the "stopped" note once the folder
  compare has finished.
- Closing several tabs and then quitting no longer goes back to asking about the other tabs, and
  files opened from an archive compare keep its extracted copy after the folder compare closes.
- The console compare exits with 2 when files could not be compared, and folders left uncompared
  (not recursive) no longer count as differences. `--numeric-tolerance` refuses NaN and infinity,
  and options that are ignored (`--patch` with three paths, `--report-diffs` without `--report` or
  for files, a text report not named `.html`) say so.
- Shortcuts of commands that do nothing in the current tab do nothing (`Ctrl+Shift+Z` no longer
  runs Undo where there is no Redo), F5 refreshes web pages shown as screenshots, the
  Filters window no longer turns the shortcuts off, files dropped while a dialog is open are
  ignored, and Enter in the command palette runs the command matching what was just typed.
- A text compare whose file changed on disk while one of its lines was being edited no longer
  says it reloaded it, and a file that could not be read when it changed is tried again instead of
  being reported as reloaded.
- `Ctrl+Tab` no longer leaves the keyboard focus on a button, which turned off the arrow keys and
  the other keys of the compare until it was clicked.
- `Ctrl+S`, closing a tab and quitting take in a line still being typed in place, instead of
  saving without it or closing without asking.
- Closing a tab left of the current one no longer switches to another tab.
- Keys no longer reach the compare behind the Add compare rule dialog.
- Copying a file or folder onto itself (both sides pointing at the same place) no longer empties
  the file; copying a folder into itself, or through a link back to itself, is refused.

## [0.9.2] - 2026-10-24

### Added

- **Command palette** (`Ctrl+K` or `Ctrl+Shift+P`): search commands, recent comparisons and
  settings in one place.
- **Find and replace** in text compares (`Ctrl+H`, `Cmd+Alt+F` on macOS), with match case, whole
  word and regular expression options. Replace all can be undone in one step.
- **Line selection** in text compares by dragging, Shift+click or Shift+arrow keys. `Ctrl+C` copies
  the selected lines, and `Alt+Right` / `Alt+Left` copies them to the other side.
- **Collapse unchanged lines** (`Alt+C`) to a clickable bar, keeping three lines of context around
  each difference, and **word wrap** (`Alt+Z`).
- **Merge arrows** on the divider between panes copy a difference in one click. The toolbar shows
  how many lines were added and removed.
- **Next file with differences:** in a file opened from a folder compare, `Alt+Down` past the last
  difference (or `Alt+Up` past the first) opens the next (or previous) file that differs, in the
  same tab.
- **Quick preview** in folder compares: press `Space` to see a colored diff of the selected file
  without opening a tab.
- **Find by name** in folder compares, with clear messages when nothing matches or every item is
  hidden by the View settings.
- **Results while comparing:** folder compares list differences and unique items as soon as they
  are found.
- A notification when a long comparison finishes while the window is in the background.
- **Binary compare:** find hex bytes (`DE AD BE EF`) or text, and go to an offset (`Ctrl+G`).
- **Image compare:** swipe mode with a draggable divider, and blink mode that alternates the
  images. Pinch to zoom and two-finger scrolling to pan.
- **Optional backup on save:** keep the previous contents as `<name>.bak` (Options → General).
- **Theme setting** with Light, Dark and Match system, and a search box in Options.
- **Reopen previous comparisons at startup** (Options → General).
- Recent comparisons on the start page can be edited or removed from their context menu.
- Dropping files on the window shows what will happen; dropping onto the Open dialog fills in the
  empty paths.
- **Windows Explorer context menu in the style of WinMerge:** an OxideMerge submenu with Compare,
  Select Left, Select Middle and Compare As, also available from the command line as
  `--select-left`, `--select-middle` and `--compare-to`.
- **Finder Quick Actions and Linux file manager entries:** Tools → Add Finder Quick Actions on
  macOS, or Add to file manager context menu on Linux (Nautilus, Nemo and Dolphin), offers
  Compare, Select Left and Select Middle for the selected items.
- **New text compare** (`Ctrl+N`): two empty sides to paste (`Ctrl+V`) or type text into, saved
  wherever you choose.
- **Paste** (`Ctrl+V`) in text compares inserts the clipboard below the current line, or replaces
  the selected lines.
- **Bookmarks** in text compares: `Ctrl+F2` sets or removes one, `F4` / `Shift+F4` jump between
  them. They are marked in the gutter and the location pane, and move with their lines as you edit.
- **Take both sides** in 3-way merges: Merge → Take left, then right (or right, then left) puts both
  changes into the result.
- **Ignore comment changes** (Options → Compare): differences only in comments don't count, for
  C-like languages, scripts, SQL, HTML/XML, CSS, Lua, Haskell and more. Also in folder compares and
  as `--ignore-comments`.
- **Format JSON and XML before comparing** (Options → Compare): files are shown re-indented, with
  JSON keys sorted, so only real content changes remain. Also in folder compares and as
  `--prettify`.
- **Apply patch** (Tools → Apply patch): applies a unified diff (plain, `git diff` or `svn diff`)
  even when lines have moved, and opens each patched file next to its original for review before
  saving. Hunks that don't fit are reported.
- **HTML report for text compares:** File → Generate report saves a side-by-side colored report
  with word differences, also from the command line with `--report`.
- **Project files:** File → Save as project stores what a tab compares (paths, filter, subfolders,
  compare method) in a `.oxmproj` file; File → Open project, the command line and drag and drop open
  it again. WinMerge `.WinMerge` projects open too.
- **More archive formats compared as folders:** `.tar`, `.tar.gz` / `.tgz`, `.tar.bz2` / `.tbz2`,
  `.tar.xz` / `.txz` and `.7z`, besides zip-based formats.
- **Git integration** instructions for using OxideMerge as `git difftool` and `git mergetool`.
- `make appimage` builds the Linux AppImage locally.
- Traditional Chinese README (`README_zh-TW.md`).

### Changed

- Large text files with many differences compare up to a hundred times faster, and no comparison
  takes more than a few seconds.
- Folder compares read large files in chunks, so identical and binary files are never loaded
  whole, and files of different sizes are told apart without reading them.
- Saving is crash-safe: a crash or a full disk never leaves a half-written file. File permissions
  and symbolic links are kept.
- The Language menu was removed from the menu bar; choose the language in Options → General.
- The pattern and command fields in Options → Plugins are wider.

### Fixed

- When saving a merge result to another file with `-o` (as `git mergetool` does), the result is
  now always written there, even if it equals one of the inputs.
- The application icon no longer shows a white fringe on dark backgrounds.

## [0.9.1] - 2026-10-23

### Added

- **Web page compare** of two or three URLs or local HTML files, in four modes: Screenshot
  (rendered with Edge, Chrome, Chromium or Brave), HTML source, visible text, and the tree of
  downloaded resources.
- **Help & Shortcuts** dialog (`F1` or `?`) listing every keyboard shortcut and mouse action, and
  an About dialog with credits.
- Files changed by another program are detected when the window regains focus: unmodified files
  reload automatically, and files with unsaved edits offer to reload.
- Keyboard selection in folder compares: arrow keys, Page Up/Down, Home/End, Shift to extend and
  `Ctrl+A` to select all.
- Save all (`Ctrl+Shift+S`), redo with `Ctrl+Shift+Z`, `Esc` to close the find bar, and a tab
  context menu to close other tabs, tabs to the right, or all tabs.
- **Release packages:** Windows `.exe` and `.zip`; Linux AppImage, `.deb` and `.tar.gz`; a
  universal macOS app (Apple silicon and Intel) in a `.dmg` and a `.zip`; and `SHA256SUMS.txt`.
- The Windows executable has an icon and version details.

### Changed

- Linux file dialogs use the XDG desktop portal, and building on Linux no longer needs
  development packages.

## [0.9.0] - 2026-10-22

First release.

- **Folder compare and merge:** 2-way and 3-way, recursive or flat, as a tree or a flat list. Six
  compare methods, file filters (wildcard masks and WinMerge `.flt` files) with a Clear filter
  option, zip-based archives compared as folders, and HTML, CSV and text reports.
- **Text compare and merge:** 2-way and 3-way with Myers, Patience or LCS, inline word and character
  differences, moved blocks, a location pane, WinMerge-style ignore options, copy and auto merge,
  undo and redo, in-place editing, conflict file resolution, syntax highlighting, find, go to line,
  CSV/TSV table mode, patches, and unpacker plugins.
- **Binary compare** in a side-by-side hex view.
- **Image compare** side by side, as an overlay or as a difference image, with highlighted
  difference blocks.
- Tabs, a start page with recent comparisons, drag and drop, light and dark themes, 10 languages,
  and the Windows Explorer context menu.
- Command-line compares, reports and patches without opening a window, and WinMerge-style switches
  on Windows.
- Chinese, Japanese and Korean fonts on Linux and WSL, and GPU acceleration under WSL2.

[0.9.8]: https://github.com/kuopinghsu/OxideMerge/compare/v0.9.7...v0.9.8
[0.9.7]: https://github.com/kuopinghsu/OxideMerge/compare/v0.9.6...v0.9.7
[0.9.6]: https://github.com/kuopinghsu/OxideMerge/compare/v0.9.5...v0.9.6
[0.9.5]: https://github.com/kuopinghsu/OxideMerge/compare/v0.9.4...v0.9.5
[0.9.4]: https://github.com/kuopinghsu/OxideMerge/compare/v0.9.3...v0.9.4
[0.9.3]: https://github.com/kuopinghsu/OxideMerge/compare/v0.9.2...v0.9.3
[0.9.2]: https://github.com/kuopinghsu/OxideMerge/compare/v0.9.1...v0.9.2
[0.9.1]: https://github.com/kuopinghsu/OxideMerge/compare/v0.9.0...v0.9.1
[0.9.0]: https://github.com/kuopinghsu/OxideMerge/releases/tag/v0.9.0
