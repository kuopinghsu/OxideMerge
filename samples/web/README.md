# Web page comparison samples

Use these three versions of a release notes page to try web page comparison without a network connection:

- `left.html`: release 2.3
- `right.html`: release 2.4
- `base.html`: release 2.2, the common base for a 3-way compare

Open them from the project folder with:

    cargo run -- samples/web/left.html samples/web/right.html
    cargo run -- samples/web/left.html samples/web/base.html samples/web/right.html

If OxideMerge is installed, replace `cargo run --` with `oxidemerge` and use the same paths.

HTML files open as web page comparisons by default; add `-t text` to compare their source
as text instead.

The pages show pictures from [`img/`](img): the logo is the same in every version, and the
search screenshot is added in 2.3 and replaced in 2.4, where a `srcset` also offers a
sharper copy for high-resolution screens.

What to look for in each mode:

- **Formatted** (the default): the pages as Markdown, shown formatted side by side:
  headings, emphasis, links, lists and the table, with the changed blocks highlighted and,
  as in Text, the changed words within them. The pages in
  [`formatted/`](#formatted-mode-samples) try it on harder content.
- **Text:** changed words are highlighted within lines. The build line differs between the
  files and a style sheet rule hides it. OxideMerge does not apply style sheets, so
  Formatted and Text show the change.
- **HTML source:** the HTML of the files, with the script but without the `<li>` items it
  adds.
- **Resources:** the files each page loads, side by side: the logo is the same, the 2.3
  screenshot is only on the left and the 2.4 screenshot and its sharper copy only on the right.

In Formatted mode, each image appears below its block with a link labeled by its alternative text.
Changes to the screenshot's alternative text and file appear as differences; the unchanged logo
does not. Text mode shows only the alternative text.

All modes read the files directly without opening a browser.
Scripts do not run, so the "Known issues" list that a script fills in is empty.

## Formatted mode samples

Three pairs of pages, each testing one topic: complex text, headings and tables. New comparisons use **Formatted** by default. If you previously chose another mode,
OxideMerge remembers it; select **Formatted** in the tab's toolbar:

    cargo run -- samples/web/formatted/text/left.html samples/web/formatted/text/right.html
    cargo run -- samples/web/formatted/headings/left.html samples/web/formatted/headings/right.html
    cargo run -- samples/web/formatted/tables/left.html samples/web/formatted/tables/right.html

or `make demo-web-text`, `make demo-web-headings` and `make demo-web-tables`. The pairs
work in the other modes too; Text shows the same changes as lines of words.

### `text/`: a style guide page

- **Inline formatting:** bold with italic inside, inline code, strikethrough and keyboard
  keys. The changed link address (`/migrate/v2` to `/migrate/v3`) counts as a change even
  though the link text is the same.
- **Characters with special meanings in Markdown:** `*`, `_`, `#`, `|`, `[brackets]` and a paragraph
  starting with `1.` stay plain text, and are not compared as changed.
- **Entities, Chinese text and line breaks:** `&mdash;`, `&copy;` and `&lt;`; a Chinese
  paragraph with a changed number; an address whose `<br>` lines stay lines.
- **Lists:** an item added to a list inside a quote, and a 3-level list where a nested
  item changes, another is added, and a step is added at the end.
- **Definition list and code block:** a changed definition, and a TOML block where two
  lines change.
- **Paragraphs:** a long paragraph where one phrase changes, and a paragraph split in two.
- **Hidden content:** a screen reader hint and the body of a closed `<details>` are not
  shown, so their changes do not count. A draft note hidden only by the style sheet is
  still shown and compared, since OxideMerge does not apply style sheets. The "Last
  reviewed" line is written by a script, so it is not compared.

### `headings/`: a user guide outline

- The title and a section are renamed (*Getting started* to *Quick start*).
- New sections are added (*Import notes*, *Conflicts*) and one is removed (*Appendix*);
  the other side shows an empty space at those positions.
- *Tags* moves above *Folders*: it shows as removed in one place and added in another.
- *Nested folders* goes from level 4 to level 3: the headings still line up, changed.
- Levels 4 to 6 are used under *Troubleshooting*, which gains inline code in its title.
- The `#` anchor links after the section titles are hidden from screen readers
  (`aria-hidden`), so they are left out.

### `tables/`: plans and reference tables

- **Plans:** a changed caption, changed prices and storage, an added row, and a cell with
  a line break.
- **Keyboard shortcuts:** an added *Linux* column; every row changes.
- **Search API parameters:** cells with code (including a `|` that must not split the
  cell), emphasis, a link and a list; a cell gains text and a row is removed.
- **Release schedule:** a cell spanning two columns (`colspan`) is replaced by two dates.
- **Contact:** a layout table without header cells; its first row is used as the header.
- **Limits:** a table nested inside a cell, with its contents combined as text.
- **Server status:** rows added by a script, so the table is empty.

Each table is one block, drawn with lines around its cells: a change anywhere in it
highlights the table on both sides. Within it, rows are lined up (by their first cell when
they changed), the changed words in each cell are highlighted, and so are whole cells of a
row or column only one side has, such as the *Family* row and the *Linux* column.
