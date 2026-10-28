# Web page compare samples

Three versions of a release notes page, for trying web page compare without a network:

- `left.html`: release 2.3
- `right.html`: release 2.4
- `base.html`: release 2.2, the common base for a 3-way compare

Open them from the project folder with:

    cargo run -- -t web samples/web/left.html samples/web/right.html
    cargo run -- -t web samples/web/left.html samples/web/base.html samples/web/right.html

or, with OxideMerge installed, `oxidemerge -t web` and the same files.

Local HTML files need `-t web`; without it they open as text.

What to look for in each mode:

- **Text:** changed words are highlighted within lines. The hidden build line (`display: none`)
  differs between the files but is not shown, because it is not on the page as you see it. The
  "Known issues" list is filled in by a script, and its changes are still there.
- **Full page:** the whole page, including the part below the first screen, with the text
  differences highlighted on the page. The note added near the top of 2.4 moves everything
  below it down, yet only the text that really changed is marked.
- **Screenshot (visible area):** the top of each page only.
- **HTML source:** the HTML after the script has run, so the generated `<li>` items are in it.

Text, Full page, Screenshot and the script-generated content need Edge, Chrome, Chromium or
Brave. Without one, or if it fails, Text and HTML source use the files as they are, and the
known issues list is empty.
