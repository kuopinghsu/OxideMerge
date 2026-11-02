# Web page compare samples

Three versions of a release notes page, for trying web page compare without a network:

- `left.html`: release 2.3
- `right.html`: release 2.4
- `base.html`: release 2.2, the common base for a 3-way compare

Open them from the project folder with:

    cargo run -- samples/web/left.html samples/web/right.html
    cargo run -- samples/web/left.html samples/web/base.html samples/web/right.html

or, with OxideMerge installed, `oxidemerge` and the same files.

HTML files open in the web page compare by default; add `-t text` to compare their source as
text instead.

What to look for in each mode:

- **Formatted** (the default): the pages as Markdown, shown formatted side by side: headings,
  emphasis, links, lists and the table, with the changed blocks highlighted and, as in Text, the
  changed words within them. The pages in [`formatted/`](#formatted-mode-samples) try it on
  harder content.
- **Text:** changed words are highlighted within lines. The build line differs between the files
  and a style sheet rule hides it. Only the screenshots apply style sheets, so Formatted and Text
  show the change.
- **Full page:** the whole page, including the part below the first screen, with the text
  differences highlighted on the page. The note added near the top of 2.4 moves everything
  below it down, yet only the text that really changed is marked.
- **Screenshot (visible area):** the top of each page only.
- **HTML source:** the HTML of the files, with the script but without the `<li>` items it adds.

Formatted, Text and HTML source convert the files as they are, without a browser, so they open at
once; the "Known issues" list that a script fills in is empty in them. Full page and Screenshot
run the script, so the list is there, and need Edge, Chrome, Chromium or Brave.

## Formatted mode samples

Three pairs of pages, each testing one topic: complex text, headings and tables. A pair opens in
**Formatted**, the default mode; if you chose another mode before, it is remembered, so pick
**Formatted** in the tab's toolbar:

    cargo run -- samples/web/formatted/text/left.html samples/web/formatted/text/right.html
    cargo run -- samples/web/formatted/headings/left.html samples/web/formatted/headings/right.html
    cargo run -- samples/web/formatted/tables/left.html samples/web/formatted/tables/right.html

or `make demo-web-text`, `make demo-web-headings` and `make demo-web-tables`. The pairs work in
the other modes too; Text shows the same changes as lines of words.

### `text/`: a style guide page

- **Inline formatting:** bold with italic inside, inline code, strikethrough and keyboard keys.
  The changed link address (`/migrate/v2` to `/migrate/v3`) counts as a change even though the
  link text is the same.
- **Characters Markdown would read:** `*`, `_`, `#`, `|`, `[brackets]` and a paragraph starting
  with `1.` stay plain text, and are not compared as changed.
- **Entities, Chinese text and line breaks:** `&mdash;`, `&copy;` and `&lt;`; a Chinese
  paragraph with a changed number; an address whose `<br>` lines stay lines.
- **Lists:** an item added to a list inside a quote, and a 3-level list where a nested item
  changes, another is added, and a step is added at the end.
- **Definition list and code block:** a changed definition, and a TOML block where two lines
  change.
- **Paragraphs:** a long paragraph where one phrase changes, and a paragraph split in two.
- **Hidden content:** a draft note hidden by the style sheet, a screen reader hint and the body
  of a closed `<details>` are not shown, so their changes do not count. The "Last reviewed"
  line is written by a script, so it is only in the screenshots.

### `headings/`: a user guide outline

- The title and a section are renamed (*Getting started* to *Quick start*).
- New sections are added (*Import notes*, *Conflicts*) and one is removed (*Appendix*); they
  face a gap on the other side.
- *Tags* moves above *Folders*: it shows as removed in one place and added in another.
- *Nested folders* goes from level 4 to level 3: the headings still line up, changed.
- Levels 4 to 6 are used under *Troubleshooting*, which gains inline code in its title.
- The `#` anchor links after the section titles are hidden from screen readers
  (`aria-hidden`), so they are left out.

### `tables/`: plans and reference tables

- **Plans:** a changed caption, changed prices and storage, an added row, and a cell with a
  line break.
- **Keyboard shortcuts:** an added *Linux* column; every row changes.
- **Search API parameters:** cells with code (including a `|` that must not split the cell),
  emphasis, a link and a list; a cell gains text and a row is removed.
- **Release schedule:** a cell spanning two columns (`colspan`) is replaced by two dates.
- **Contact:** a layout table without header cells; its first row is used as the header.
- **Limits:** a table nested in a cell, whose cells run together as text.
- **Server status:** rows added by a script, so the table is empty except in the screenshots.

Each table is one block, drawn with lines around its cells: a change anywhere in it highlights
the table on both sides. Within it, rows are lined up (by their first cell when they changed),
the changed words in each cell are highlighted, and so are whole cells of a row or column only
one side has, such as the *Family* row and the *Linux* column.
