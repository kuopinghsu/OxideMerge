# Sample comparisons

Files for trying OxideMerge's features without your own data. Run the commands from the project
folder: `cargo run --` runs the debug build; with OxideMerge installed, use `oxidemerge` instead.

## Folder compare: `left` and `right`

    cargo run -- -r samples/left samples/right      # or: make demo

The two folders differ in every way a folder compare shows: changed text and binary files,
identical files (`same.txt`, `src/util/strings.rs`), and files and folders on one side only
(`only_left/`, `docs/`). Select files by name with patterns such as `*.log; *.py`, open the
**All differences** view, or write a report or slideshow from the **File** menu.

Each pair below can also be opened on its own, for example
`cargo run -- samples/left/src/inventory.py samples/right/src/inventory.py`.

| Files | What to try |
|---|---|
| `src/main.rs` | A small text compare (`make demo-text`). |
| `src/inventory.py` | Changes within lines, added lines, and the `restock` function moved up: turn on **Detect moved lines** to see it as a move. Set a synchronization point (Alt+S on a line in each pane) to line up the two `restock` functions. |
| `src/queue.c` | Only comments and indentation changed (spaces against tabs). With **Ignore comment changes** and **Whitespace: Ignore all** the files compare equal. |
| `config/app.json` | Left is one line, right is formatted with its keys in another order. With **Format JSON and XML before comparing** (`--prettify`) only the two changed values remain. |
| `config/layout.xml` | The same for XML: formatted, only the added `<column>` remains. |
| `data/measurements.tsv` | Table view of tab-separated data, with a quoted note that spans two lines. With **Treat numbers as equal within** `0.001` (`--numeric-tolerance 0.001`), the readings that moved by less than that are ignored; the temperature that changed by 1, the edited note and the new sensor remain. |
| `logs/build.log` | Timestamps, a load address, a generated header and a license block that differ on every build. Compare with the rules file `samples/rules/Build logs.oxrules` (`--rules "samples/rules/Build logs.oxrules"`, or import it under **Options → Compare rules**) and only the changed test result remains. |
| `logs/large.log` | 2,000 lines with a few scattered changes: use the location pane and Next/Previous difference to move between them. |
| `notes/release-notes.md` | A longer document with several separate changes, for HTML reports and slideshows (`--report notes.html --slideshow`). |
| `text/case-and-numbers.txt` | Changes of letter case, numbers and blank lines only: equal with **Ignore case**, **Ignore numbers** and **Ignore blank lines**. |
| `text/line-endings.txt` | The same text with Windows (CRLF) and Unix (LF) line endings. It compares equal by default; turn off **Ignore carriage return differences** to see every line differ. |
| `text/encoding.txt` | The same text in ISO-8859-1 and in UTF-8 with a byte order mark. It compares equal by default; turn off **Ignore code page differences** to see the difference. |
| `images/chart.png` | A bar chart where one bar grew and two changed color. Try the side by side, overlay, swipe, blink and difference views. |
| `images/gradient.ppm`, `images/stamp.pbm` | Netpbm images (binary PPM and plain-text PBM) with a few changed areas. Zoom in on the small stamp. |
| `bin/firmware.img` | A 4 KiB binary with a changed header field, changed bytes and 16 inserted bytes, in the hexadecimal view. |
| `archives/bundle.zip` | Open it from the folder compare to compare the archives as folders: changed files and one added file. |
| `blob.bin`, `picture.png`, `data.csv` | The original small binary, image and CSV samples. |

## Three-way compare: `three`

    cargo run -- samples/three/left.txt samples/three/base.txt samples/three/right.txt   # make demo-3way
    cargo run -- samples/three/settings/left.ini samples/three/settings/base.ini samples/three/settings/right.ini   # make demo-3way-merge
    cargo run -- -r samples/three/folders/left samples/three/folders/base samples/three/folders/right   # make demo-3way-folder
    cargo run -- samples/three/conflict.txt                                              # make demo-conflict
    cargo run -- samples/three/diff3-conflict.py

- `settings/*.ini`: changes made on the left only, on the right only, the same change on both
  sides, and conflicting changes. **Auto merge** takes the first three kinds and leaves the
  conflicts for you to resolve.
- `folders/`: a 3-way folder compare with a file changed on one side, on the other, the same on
  both, in conflict, added on the left, and removed on the right. Use **Auto merge** on the folder
  compare too.
- `conflict.txt` and `diff3-conflict.py`: files with version-control conflict markers. The second
  has two conflicts in Git's diff3 style, with the base version between `|||||||` and `=======`,
  so it opens as a 3-way merge.

## Web page compare: `web`

See [web/README.md](web/README.md).
