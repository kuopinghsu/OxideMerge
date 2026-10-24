# OxideMerge

[English](README.md) | 繁體中文

![OxideMerge：跨平台的差異比較與合併工具](images/banner.jpg)

OxideMerge 是以 Rust 撰寫的開放原始碼跨平台差異比較與合併工具。它將
[WinMerge](https://winmerge.org/)（[原始碼](https://github.com/WinMerge/winmerge)）的功能帶到
**Windows、macOS 與 Linux**，可以比較並合併資料夾、文字檔、二進位檔與影像，也能比較網頁，並提供現代化的多語系介面。

名稱源自撰寫它的程式語言 Rust：鐵鏽（rust）的成分正是氧化鐵（iron oxide）。

![起始頁](images/start.png)

| 資料夾比較 | 文字比較 |
| --- | --- |
| ![資料夾比較](images/folder.png) | ![文字比較](images/text.png) |

## 功能

**資料夾比較與合併**
- 支援雙向與三向資料夾比較，可選擇是否包含子資料夾，並以樹狀檢視或平面清單呈現。
- 比較進行中就會顯示結果：掃描資料夾的同時即比較檔案，一發現差異或單側獨有的項目便立即列出。
- 大型檔案會分段比較，相同的檔案與二進位檔絕不會整個載入記憶體；使用「快速內容」與「二進位內容」比較方法時，大小不同的檔案不必讀取內容即可判定為不同。
- 比較方法：完整內容、快速內容、二進位內容、修改日期、修改日期與大小、僅比較大小。
- 可分別顯示或隱藏相同、不同與單側獨有的項目。可在兩側之間複製或刪除項目，並可選擇使用資源回收筒及顯示確認提示。
- 可將任一側所選的項目複製到其他資料夾並保留相對路徑，也可用萬用字元（`*.c; *.h`）依名稱選取項目。
- 資料夾清單下方的檔案比較（「檢視」選單）：所選檔案的差異直接顯示在清單下方，不必另開分頁。
- 三向資料夾自動合併到中間（共同祖先）資料夾：只在一側所做的變更會複製過去、刪除會一併套用，兩側都有變更的文字檔若變更沒有重疊，會逐行合併。衝突會列出並維持原狀。
- 鍵盤選取：方向鍵、Page Up／Page Down、Home／End，按住 Shift 可擴大選取範圍，按 Ctrl+A 全選。
- 快速預覽：按空白鍵即可查看所選檔案以色彩標示的差異，不必另開分頁。
- 視窗在背景時若耗時較久的比較已完成，會發出通知提醒你。
- 檔案篩選器：支援 `*.c;*.h;!*.bak;!.git\` 這類萬用字元條件，以及 WinMerge 的 `.flt` 篩選器檔案。
- 壓縮檔會先解壓縮，再當成資料夾比較：支援以 Zip 為基礎的格式（`.zip`、`.jar`、`.apk`、`.docx`、`.xlsx`、`.odt`、`.epub`、`.nupkg` 等）、`.tar`、`.tar.gz`／`.tgz`、`.tar.bz2`／`.tbz2`、`.tar.xz`／`.txz` 與 `.7z`。
- 可產生 HTML、CSV 與純文字格式的報告。HTML 報告也可附上每個有變更的文字檔的左右並排差異（「檔案 → 產生含檔案差異的報告」）。
- 統計資料（「工具」選單）：相同、不同與單側獨有的檔案數量，以及總大小。
- 專案檔（`.oxmproj`）可儲存分頁所比較的內容（路徑、篩選器、子資料夾與比較方式），之後再次開啟；也可開啟 WinMerge 的 `.WinMerge` 專案檔。

**文字比較與合併**
- 雙向與三向比較，可選用 Myers、Patience 或 LCS 差異演算法。大型檔案會先以兩側各只出現一次的行切分，即使有數萬處差異，也能在一秒內完成比較。
- 行內差異（字詞與字元層級）、搬移區塊偵測，以及可綜覽全部差異的位置窗格。
- 可將未變更的行摺疊成一列，點按即可展開（每個差異前後保留三行作為上下文），長行也可自動換行。
- 將指標移到差異上，即可點按兩欄分隔線上的箭頭直接複製；工具列會顯示新增與刪除的行數。
- 與 WinMerge 相同的忽略選項：空白字元（僅忽略變動或全部忽略）、大小寫、空白行、換行字元格式、數字及字碼頁。
- 數字容許誤差：差距不超過設定值（例如 `0.001`）的數字視為相同，適合比較量測值與產生的資料。
- 區塊篩選器：忽略符合規則運算式的起始行與結束行之間的所有內容，例如自動產生的區段或授權標頭。
- 可忽略註解變更（適用於已知註解語法的語言：C 類語言、指令碼、SQL、HTML／XML、CSS、Lua、Haskell 等），也可在比較前格式化 JSON 與 XML（重新縮排並排序 JSON 鍵），只留下真正的內容變更。這兩個選項也適用於資料夾比較。
- 「檔案 → 新增文字比較」（Ctrl+N）會開啟兩個空白側，可貼上（Ctrl+V）或輸入文字；在任何可編輯的一側按 Ctrl+V，會將剪貼簿內容插入目前這一行下方，或取代選取的行。
- 書籤：按 Ctrl+F2 加入或移除，按 F4／Shift+F4 跳到下一個／上一個；書籤會標示在行號欄與位置窗格中。書籤可加上註解（「編輯 → 書籤註解」），指標移到行號欄時會顯示，也會出現在 HTML 報告中。
- 儲存比較（「檔案」選單）：將每一側的文字連同書籤與註解存成一個 `.oxmcmp` 檔，方便日後檢視或傳給他人。
- 與 Git 修訂版本比較（「檔案」選單或 `--git-rev`）：將檔案或資料夾與其在 `HEAD`、分支、標籤或提交中的內容比較。
- 行詳細資料窗格（「檢視」選單）：將每一側的目前行上下排列並自動換行，方便找出長行中的細微差異。
- 行號欄的編輯標記會顯示開啟檔案後變更過的行。
- 統計資料（「工具」選單）：每一對比較側的相同、變更、新增、移除與忽略的行數。
- 增加或減少選取行的縮排（Ctrl+]／Ctrl+[）。
- 三向合併可採用差異的兩側（先左後右，或先右後左）。
- 可將差異複製到左側或右側、全部複製、在三向比較中自動合併、復原與重做，並可直接編輯內容。
- 可用拖曳、Shift＋點按或 Shift＋方向鍵選取多行；按 Ctrl+C 複製到剪貼簿，或按 Alt+Right／Alt+Left 複製到另一側。
- 可解決含有 `<<<<<<<`、`|||||||`、`=======` 與 `>>>>>>>` 標記的版本控制衝突檔案。
- 自動偵測編碼與換行字元，並提供語法醒目提示、跳至指定行，以及 CSV／TSV 表格模式。
- 尋找與取代，可選擇大小寫須相符、全字拼寫須符合及規則運算式（取代文字中的 `$1` 代表擷取的群組）；全部取代只算一個復原步驟。
- 存檔時會先寫入暫存檔，再以更名方式取代原檔，因此即使當機或磁碟已滿，也不會留下寫到一半的檔案；也可選擇將先前的內容保留為 `<檔名>.bak`。
- 可產生一般、上下文（context）與統一（unified）格式的修補檔，以及左右並排的 HTML 報告。
- 套用修補檔（工具選單）：套用統一格式的差異檔（一般、`git diff` 或 `svn diff`），即使行已移動也能找到每個區塊，並將每個修補後的檔案與原檔並排開啟，供你檢查後再儲存。無法套用的區塊會列出來。
- 視窗重新取得焦點時，會偵測遭其他程式修改的檔案：未修改的檔案會自動重新載入，有尚未儲存之編輯的檔案則會顯示提示列，讓你選擇是否重新載入。
- 解譯器外掛程式：比較前先執行外部指令（例如 `pandoc -t plain ${file}`），將檔案轉換為文字。

**二進位比較**
- 左右並排的十六進位檢視，標示位元組層級的差異，並支援差異瀏覽與同步捲動。
- 可在任一窗格中尋找十六進位位元組（`DE AD BE EF`）或文字，並可跳至指定位移（`0x1F00`、`1F00h` 或十進位）。

**影像比較**
- 並排、疊加、滑動、閃爍與差異五種模式，可縮放、符合視窗或以實際大小（1:1）檢視。滑動模式以可拖曳的分隔線左右各顯示一張影像；閃爍模式則讓兩張影像輪流顯示。
- 以滑鼠滾輪或雙指縮放手勢，依指標位置縮放；拖曳或以雙指滑動即可平移。
- 以可調整的色彩門檻與區塊大小標示差異區塊。

**網頁比較**
- 可比較兩個或三個 `http://`、`https://` 網址或本機 HTML 檔案，共有四種模式：
  - **螢幕截圖**：以無介面模式的 Chromium 系列瀏覽器（Edge、Chrome、Chromium 或 Brave，會自動尋找）繪製各個網頁，再以影像比較開啟截圖。
  - **HTML 原始碼**：網頁的原始碼，經過壓縮而過長的單行會在標籤之間斷開，以文字比較開啟。
  - **文字**：各網頁中看得見的文字（已移除指令碼、樣式與標記），以文字比較開啟。
  - **資源樹**：下載各網頁及其樣式表、指令碼、圖片、框架與多媒體檔案，再以資料夾比較來比較下載的檔案。
- 依 HTTP 標頭或 `<meta>` 標籤指定的字元集解碼網頁。HTTP 狀態碼不是 2xx 時，會顯示在窗格標題列。

**介面**
- 分頁式文件、列出最近比較紀錄的起始頁，並支援拖放檔案與資料夾。
- 淺色與深色主題。
- 內建 10 種語言：English、繁體中文、简体中文、日本語、한국어、Deutsch、Français、Español、Русский 與 Português (Brasil)。你也可以用 JSON 檔案新增其他語言（請參閱[語言](#語言)）。
- 仿照 WinMerge 的 Windows 檔案總管右鍵選單：可比較選取的檔案或資料夾，也可以先選定左側（及中間）項目，之後再與其比較。macOS 的 Finder 快速動作，以及 Linux 的 Nautilus、Nemo 與 Dolphin 也提供相同的項目。

## 支援平台

| 平台 | 說明 |
| --- | --- |
| Windows 10／11 | 檔案總管右鍵選單、WinMerge 風格的 `/switch` 參數、資源回收筒 |
| macOS 11 以上 | 原生檔案對話框、垃圾桶、「在 Finder 中顯示」、Finder 快速動作 |
| Linux（X11 與 Wayland） | 透過 XDG 桌面入口（desktop portal）開啟檔案對話框、freedesktop 垃圾桶、Nautilus／Nemo／Dolphin 右鍵選單 |

三個平台使用同一套程式碼建置。字型取自系統：Windows 使用 Segoe UI 與 Cascadia Mono，macOS 使用 SF 與 Menlo，Linux 則使用 Noto、Inter 或 DejaVu。在 Linux 上顯示中文、日文與韓文時，會使用 fontconfig 能找到的任何中日韓字型（建議安裝 Noto Sans CJK 或文泉驛）；在 WSL 下若未安裝 Linux 的中日韓字型，則會改用 Windows 的字型。

在 WSL2 下，若已安裝 Mesa 的 `d3d12` 驅動程式，程式會選用它（透過 WSLg 使用 GPU 加速），並隱藏 Mesa 在偵測 EGL 驅動程式時輸出的警告。如需變更這兩項行為，請自行設定 `GALLIUM_DRIVER` 或 `EGL_LOG_LEVEL` 環境變數。

## 安裝

每個 [GitHub 發行版本](https://github.com/kuopinghsu/OxideMerge/releases)都提供下列安裝套件，並附上 `SHA256SUMS.txt` 供你核對檔案：

| 平台 | 安裝套件 |
| --- | --- |
| Windows x64 | `oxidemerge-<版本>-windows-x64.exe`（可直接執行）或 `.zip` |
| Linux x64 | `.AppImage`（Ubuntu 20.04、RHEL 8、Debian 10 及更新版本，免安裝）、`.deb`（Ubuntu 22.04 及更新版本）或 `.tar.gz` |
| macOS 11 以上（Apple 晶片與 Intel） | `oxidemerge-<版本>-macos-universal.dmg`，或內含同一個應用程式的 `.zip` |

- **Linux：** 啟動 AppImage 前，請先執行 `chmod +x oxidemerge-*.AppImage`。若系統沒有 FUSE，請加上 `--appimage-extract-and-run` 參數啟動。安裝 Debian 套件請執行 `sudo apt install ./oxidemerge-*.deb`。檔案對話框需要 `xdg-desktop-portal`，一般桌面環境都已內建。
- **macOS：** 本應用程式僅經過臨時簽署（ad-hoc），未經 Apple 公證。第一次開啟時，請按住 Control 鍵再點按應用程式並選擇「**打開**」，或在「**系統設定 → 隱私權與安全性**」中允許執行。若要在終端機中使用，請執行
  `sudo ln -s /Applications/OxideMerge.app/Contents/MacOS/oxidemerge /usr/local/bin/oxidemerge`。

## 建置

你需要安裝 Rust 工具鏈（[rustup](https://rustup.rs/)）；在 Linux 上還需要 C 編譯器。OxideMerge 會在執行時才載入 X11、Wayland、鍵盤與 OpenGL 程式庫，因此建置時不需要任何開發套件。

接著執行建置：

```sh
cargo build --release          # 執行檔：target/release/oxidemerge(.exe)
cargo test
```

### Make 目標

專案提供 GNU Make 使用的 `Makefile`，包裝了常用工作，在 Windows 與 Unix 上皆可使用。

| 目標 | 說明 |
| --- | --- |
| `make` / `make release` | 最佳化的正式版建置 |
| `make build` | 偵錯版建置 |
| `make run ARGS="a.txt b.txt"` | 帶參數執行偵錯版 |
| `make demo` | 比較 `samples/left` 與 `samples/right` 資料夾 |
| `make demo-text` / `demo-3way` / `demo-conflict` | 文字、三向與衝突檔案示範 |
| `make test` / `check` / `lint` | 單元測試、型別檢查、將警告視為錯誤的 clippy 靜態檢查 |
| `make ci` | 依序執行 `check`、`lint` 與 `test` |
| `make check-platforms` | 針對 Windows、macOS 與 Linux 進行型別檢查（每個平台都需先以 `rustup target add` 加入目標） |
| `make install` / `uninstall` | 以 `cargo install` 安裝到 `~/.cargo/bin`，或解除安裝 |
| `make shell-register` | 安裝後加入檔案管理員項目（檔案總管、Finder 快速動作、Nautilus、Nemo、Dolphin） |
| `make dist` | 將正式版執行檔打包到 `dist/`（Windows 為 `.zip`，其他平台為 `.tar.gz`） |
| `make appimage` | 在 `dist/` 產生 Linux x86_64 AppImage（結構與 CI 相同；需已安裝 X11、Wayland 與鍵盤程式庫，並會下載 `appimagetool`） |
| `make clean` | 刪除建置產出 |
| `make help` | 列出所有目標 |

### 持續整合

每次推送到 `main` 分支及每個拉取要求（pull request），都會執行 [`.github/workflows/ci.yml`](.github/workflows/ci.yml)。它會在 Windows、Linux 與 macOS 上執行將警告視為錯誤的 clippy 靜態檢查、執行單元測試、建置正式版，並對指令列功能進行冒煙測試（`--version`、`--list-languages` 與 `--cli` 的結束代碼），接著將下列安裝套件上傳為工作流程成品：

- **Windows：** `.exe` 與 `.zip`。
- **Linux：** 在 Ubuntu 22.04 上建置的 `.tar.gz` 與 `.deb`。
- **Linux AppImage：** 在 AlmaLinux 8（glibc 2.28）上建置，內含 X11、Wayland 與鍵盤程式庫，而 [`assets/AppRun`](assets/AppRun) 只會在系統缺少這些程式庫時才使用內附的版本。建置完成後，會在 Ubuntu 22.04、24.04 及 AlmaLinux 8、9 上檢查這個 AppImage：每個內附的程式庫都必須能正確解析，指令列必須能正常運作，視窗也必須能在 Xvfb 下持續開啟。
- **macOS：** 經臨時簽署的通用版（Apple 晶片與 Intel）`OxideMerge.app`，分別包裝成 `.dmg` 與 `.zip`。

推送 `v0.9.2` 這類標籤時，會在 GitHub 發行版本頁面發布所有安裝套件與 `SHA256SUMS.txt`。標籤必須與 `Cargo.toml` 中的版本一致；含有 `-` 的標籤（例如 `v1.0.0-rc1`）會標示為搶先版。發行說明取自 [`CHANGELOG.md`](CHANGELOG.md) 中該版本的段落，後面再附上下載說明，因此請在推送標籤前先寫好該段落；缺少時建置會提早失敗。執行 `scripts/release-notes.sh 0.9.2` 可預覽某個版本的發行說明。CI 只會讀取 `CHANGELOG.md`，不會修改它。

發行新版本的步驟：

1. 修改 `Cargo.toml` 中的 `version`，並執行 `cargo build` 讓 `Cargo.lock` 一併更新。
2. 在 `CHANGELOG.md` 中，把該版本的標題從 `## [0.9.2] - Unreleased` 改成發行日期，例如 `## [0.9.2] - 2026-10-24`，並檢查內容。
3. 執行 `make fmt-check ci`，再用 `scripts/release-notes.sh 0.9.2` 預覽發行說明。
4. 提交並推送到 `main`，等待所有平台的 CI 都通過。標籤會建置它所指向的提交，若該提交的 CI 失敗，發行也會失敗。
5. 為該提交加上標籤並推送：

   ```sh
   git tag -a v0.9.2 -m "OxideMerge 0.9.2"
   git push origin v0.9.2
   ```

6. 開始下一個版本：在 `CHANGELOG.md` 最上方加入 `## [0.9.3] - Unreleased` 段落，之後的變更都記錄在這裡。

若發行建置失敗，請先在 `main` 上修正，再把標籤移到新的提交並重新推送（`git tag -f -a v0.9.2 -m "OxideMerge 0.9.2"` 與 `git push -f origin v0.9.2`）。

## 使用方式

不帶任何參數執行 `oxidemerge` 會開啟起始頁。你可以從起始頁開啟檔案或資料夾、將它們拖放到視窗中，或選擇最近的比較紀錄。

```sh
oxidemerge left.txt right.txt                   # 雙向文字比較
oxidemerge mine.txt base.txt theirs.txt         # 三向比較（中間為共同基礎版本）
oxidemerge -r C:\project\v1 C:\project\v2       # 包含子資料夾的資料夾比較
oxidemerge -r -f "*.rs;!target/" old new        # 套用篩選器的資料夾比較
oxidemerge conflict.txt                         # 解決含衝突標記的檔案
oxidemerge -t image a.png b.png                 # 指定比較類型
oxidemerge https://example.com https://example.org   # 網頁比較
oxidemerge -t web old.html new.html             # 將本機 HTML 檔案當成網頁比較
oxidemerge left.txt right.txt -o merged.txt     # 將合併結果儲存到另一個檔案
oxidemerge release.oxmproj                      # 開啟已儲存的專案（或 .WinMerge 專案）
oxidemerge -r old.tar.gz new.tar.gz             # 將壓縮檔當成資料夾比較
oxidemerge --git-rev HEAD~1 src/main.rs         # 將檔案與較早的 Git 修訂版本比較
oxidemerge review.oxmcmp                        # 開啟已儲存的比較及其書籤
```

網頁比較分頁上有模式列（螢幕截圖、HTML 原始碼、文字、資源樹）與重新載入按鈕，並會記住上次使用的模式。螢幕截圖的尺寸與瀏覽器路徑可在「**選項 → 影像 / 二進位 / 網頁**」中設定；瀏覽器路徑留白時會自動偵測。

### 指令列選項

| 選項 | 說明 |
| --- | --- |
| `PATHS...` | 2 或 3 個檔案、資料夾或網址，或單一衝突檔案、專案檔或已儲存的比較 |
| `-r, --recursive` | 資料夾比較時包含子資料夾 |
| `-f, --filter <MASK>` | 檔案篩選條件，例如 `*.c;*.h` 或 `!*.bak` |
| `-m, --method <M>` | 資料夾比較方法：`full`、`quick`、`binary`、`date`、`datesize`、`size` |
| `-t, --compare-as <T>` | 指定以 `text`、`binary`、`image` 或 `web` 方式比較 |
| `-o, --output <PATH>` | 合併結果的輸出路徑 |
| `--dl/--dm/--dr <TEXT>` | 取代左側、中間與右側路徑顯示的說明文字 |
| `--wl/--wm/--wr` | 以唯讀方式開啟左側、中間或右側 |
| `--ignore-ws <MODE>` | 忽略空白字元：`compare`、`change` 或 `all` |
| `--ignore-case`、`--ignore-blank`、`--ignore-eol`、`--ignore-numbers` | 忽略大小寫、空白行、換行字元、數字 |
| `--ignore-comments` | 忽略註解中的變更 |
| `--prettify` | 比較前重新格式化 JSON 與 XML |
| `--numeric-tolerance <AMOUNT>` | 差距不超過此值的數字視為相同 |
| `--git-rev <REV>` | 將指定的單一路徑與其在 Git 修訂版本中的內容比較 |
| `--report <FILE>` | 輸出報告：資料夾比較為 `.html`、`.csv` 或 `.txt`，文字比較為左右並排的 `.html` |
| `--report-diffs` | 在 HTML 資料夾報告中附上每個有變更的文字檔的差異 |
| `--patch <FILE>` | 為兩個檔案或資料夾輸出修補檔 |
| `--patch-style <S>` | `normal`、`context` 或 `unified`（預設） |
| `--context <N>` | 修補檔的上下文行數（預設為 3） |
| `--cli` | 不開啟視窗執行；結束代碼 0 表示相同、1 表示不同、2 表示發生錯誤 |
| `--lang <CODE>` | 介面語言，例如 `ja` 或 `zh-TW` |
| `--list-languages` | 列出可用的語言 |
| `--register-shell` / `--unregister-shell` | 加入或移除檔案管理員項目（檔案總管、Finder、Nautilus、Nemo、Dolphin） |
| `--select-left <PATH>` / `--select-middle <PATH>` | 記住某一側的項目，供之後的 `--compare-to` 使用 |
| `--compare-to <PATH>` | 將指定路徑與先前記住的左側（及中間）項目比較 |

在 Windows 上也接受 WinMerge 風格的參數，例如 `/r`、`/f`、`/o`、`/dl`、`/dr`、`/wl`、`/wr` 等，因此既有的指令碼不必修改即可沿用。

### 撰寫指令碼

```sh
# 只取得結束代碼，不開啟視窗
oxidemerge --cli -r dirA dirB && echo identical

# 產生資料夾比較的 HTML 報告（使用 --report 與 --patch 時一律不開啟視窗）
oxidemerge -r dirA dirB --report report.html

# HTML 資料夾報告，後面附上每個有變更檔案的差異
oxidemerge -r dirA dirB --report report.html --report-diffs

# 兩個檔案左右並排的 HTML 報告，並忽略註解變更
oxidemerge old.c new.c --ignore-comments --report diff.html

# 產生含 5 行上下文的統一格式差異
oxidemerge old.c new.c --patch changes.patch --context 5

# 比較兩個網頁中看得見的文字（若上次使用的模式是 HTML 原始碼，則改為比較原始碼）
oxidemerge --cli https://staging.example.com/ https://www.example.com/
```

### Git 整合

將 OxideMerge 設為 Git 的比對與合併工具：

```sh
git config --global diff.tool oxidemerge
git config --global difftool.oxidemerge.cmd 'oxidemerge "$LOCAL" "$REMOTE"'
git config --global merge.tool oxidemerge
git config --global mergetool.oxidemerge.cmd \
  'oxidemerge "$LOCAL" "$BASE" "$REMOTE" -o "$MERGED" --dl 本地 --dr 遠端 --wl --wr'
git config --global mergetool.oxidemerge.trustExitCode false
```

- `git difftool` 會逐一開啟每個變更的檔案；`git difftool --dir-diff` 則以一個資料夾比對開啟全部檔案，在最後一個差異按 Alt+Down 會接著開啟下一個變更的檔案。
- `git mergetool` 會開啟三方合併，中間是共同祖先。使用自動合併（或把差異複製到中間）後存檔：中間窗格會寫入合併結果檔，不會改動共同祖先。未存檔就關閉時，衝突會維持未解決。
- 在 macOS 上，除非已將程式連結到 `PATH`（請參閱安裝一節），請使用完整路徑 `/Applications/OxideMerge.app/Contents/MacOS/oxidemerge`；在 Windows 上請使用 `oxidemerge.exe` 的路徑。

### 快速鍵

按 `F1` 或 `?`，或點按右上角的 `?` 按鈕，即可開啟「說明與快速鍵」對話框。其中依檢視分類列出所有快速鍵與滑鼠操作，並顯示設定檔的儲存位置。

| 按鍵 | 動作 |
| --- | --- |
| `F1`、`?` | 顯示或隱藏「說明與快速鍵」 |
| `Ctrl+K`、`Ctrl+Shift+P` | 命令面板：搜尋命令、最近的比較和設定 |
| `Ctrl+N` | 新增文字比較（兩個空白側） |
| `Ctrl+O` | 開啟 |
| `Ctrl+S` | 儲存 |
| `Ctrl+Shift+S` | 全部儲存 |
| `Ctrl+W`、`Ctrl+F4` | 關閉分頁（在分頁上按右鍵，可關閉其他分頁、右側的分頁或全部分頁） |
| `Ctrl+Tab`、`Ctrl+Shift+Tab` | 下一個或上一個分頁 |
| `Ctrl+F`、`F3`、`Shift+F3`、`Esc` | 尋找、尋找下一個、尋找上一個、關閉尋找列 |
| `Ctrl+H`（macOS 上為 `Cmd+Alt+F`） | 尋找與取代 |
| `Ctrl+G` | 跳至指定行（二進位比較中為跳至位移） |
| `Shift+Up`、`Shift+Down`、`Shift+PgUp`、`Shift+PgDn` | 選取多行 |
| `Ctrl+C` | 複製選取的行 |
| `Ctrl+V` | 貼到目前這一行下方，或取代選取的行 |
| `Ctrl+F2`、`F4`、`Shift+F4` | 加入或移除書籤、下一個書籤、上一個書籤 |
| `F5` | 重新整理 |
| `Alt+Down`、`Alt+Up` | 下一個或上一個差異；從資料夾比對開啟的檔案中，越過最後或第一個差異時，會開啟下一個或上一個有差異的檔案 |
| `Alt+Home`、`Alt+End` | 第一個或最後一個差異 |
| `Alt+PgDn`、`Alt+PgUp` | 下一個或上一個衝突 |
| `Alt+Right`、`Alt+Left` | 將目前的差異或選取的行複製到右側或左側 |
| `Ctrl+Alt+Right`、`Ctrl+Alt+Left` | 將所有差異複製到右側或左側 |
| `Ctrl+Z`、`Ctrl+Y` / `Ctrl+Shift+Z` | 復原、重做 |
| `F2`、`Enter` | 編輯目前這一行 |
| `Ctrl+]`、`Ctrl+[` | 增加或減少選取行的縮排 |
| `Alt+C`、`Alt+Z` | 摺疊未變更的行、自動換行 |
| `Space` | 在資料夾比較中快速預覽所選檔案 |
| `Ctrl++`、`Ctrl+-`、`Ctrl+0` | 放大、縮小、重設縮放比例 |

## 設定

選項會以 JSON 格式儲存在使用者設定資料夾中：

- Windows：`%APPDATA%\OxideMerge\config.json`
- macOS：`~/Library/Application Support/OxideMerge/config.json`
- Linux：`~/.config/OxideMerge/config.json`

若要做成可攜式版本，請在執行檔所在的資料夾中建立一個空白的 `oxidemerge.json`，OxideMerge 便會改將選項儲存在這個檔案中。

### 篩選器

多個篩選條件以 `;` 分隔。開頭加上 `!` 表示排除符合的項目，結尾加上 `\` 或 `/` 則表示比對資料夾，例如：`*.rs;*.toml;!target\`。放在設定資料夾下 `Filters` 子資料夾中的 WinMerge `.flt` 篩選器檔案，會以 `[F] <名稱>` 的形式出現在篩選器清單中。

### 語言

內建的語言套件位於 [`lang/`](lang) 資料夾，並會嵌入執行檔中。若要新增或覆寫某種語言，請將 `lang/en.json` 複製為 `<語言代碼>.json`，翻譯其中的值（所有 `{預留位置}` 名稱請保持不變），再將檔案放到執行檔旁的 `Languages` 資料夾或設定資料夾中。單元測試會檢查每個內建語言套件是否包含所有鍵值，且保留了所有預留位置。

### 檔案總管整合（Windows）

執行 `oxidemerge --register-shell`（或使用「**工具 → 加入檔案總管右鍵選單**」），即可為目前的使用者在檔案、資料夾、磁碟機及資料夾空白處的檔案總管右鍵選單中，加入 **OxideMerge** 子選單。其運作方式與 WinMerge 的殼層擴充功能相同：

- **比較**：選取兩個或三個檔案或資料夾，在同一個視窗中比較（選取三個時為三向比較）。若只選取一個項目，則會與先前以「選為左側」（及「選為中間」）選定的項目比較；若未曾選定，則會開啟「選擇檔案或資料夾」對話框並預先填入該項目。
- **選為左側**／**選為中間**：記住某個項目（例如位於另一個資料夾中的項目）。之後「比較」項目會改為顯示「**與「名稱」比較**」。
- **比較方式**（僅限檔案）：以文字、二進位、影像或網頁方式比較。

檔案總管會為每個選取的項目各啟動一個 OxideMerge 程序；這些程序會透過暫存資料夾中的一個小型佇列資料夾找到彼此，且只有第一個程序會開啟視窗。選單項目會指向你執行註冊指令時所用的執行檔，因此請從已安裝的版本執行（`make shell-register` 會自動處理），若之後搬移了程式，也請重新執行一次。在 Windows 11 上，這些項目位於「**顯示其他選項**」之下。

### Finder 與 Linux 檔案管理員整合

在 macOS 上，使用「**工具 → 加入 Finder 快速動作**」（或執行 `oxidemerge --register-shell`），即可在 Finder 的快速動作與「服務」選單中加入 **OxideMerge: Compare**、**OxideMerge: Select Left** 與 **OxideMerge: Select Middle**。這些項目是位於 `~/Library/Services` 的 Automator 工作流程。

在 Linux 上，使用「**工具 → 加入檔案管理員右鍵選單**」，即可在已安裝的檔案管理員中加入相同的三個項目：Nautilus 指令碼子選單（`~/.local/share/nautilus/scripts`）、Nemo 動作（`~/.local/share/nemo/actions`）與 Dolphin 服務選單（`~/.local/share/kio/servicemenus`）。

這些項目的運作方式與檔案總管相同：Compare 會比較選取的兩個或三個項目，或將單一項目與先前以 Select Left／Select Middle 選定的一側比較。與 Windows 相同，選單項目會指向你執行註冊時所用的執行檔。

## 專案結構

```
src/
  main.rs           指令列解析與程式啟動
  config.rs         保存的選項
  i18n.rs           語言套件
  shell.rs          檔案總管右鍵選單、Finder 快速動作、Linux 檔案管理員項目
  core/             差異比較引擎、資料夾掃描、二進位／影像／表格比較、
                    衝突解析、篩選器、壓縮檔、修補檔、報告、專案檔、
                    註解剝除、JSON／XML 格式化、
                    網頁下載、文字擷取與螢幕截圖
  ui/               egui 應用程式、各種檢視、主題與介面元件
build.rs            將圖示與版本資訊嵌入 Windows 執行檔
assets/             應用程式圖示（.png、.ico）、Linux 桌面項目檔與 AppImage 啟動程式
scripts/            `make appimage` 與 CI 使用的 Linux AppImage 打包指令碼，以及從 CHANGELOG.md 擷取發行說明的指令碼
lang/               內建翻譯
samples/            make 示範目標使用的檔案
```

## 限制

- 壓縮檔支援以 Zip 為基礎的格式、tar（未壓縮、gzip、bzip2、xz）與 7z，不支援 rar。
- 格式化後的 JSON 與 XML 以唯讀方式顯示；若要編輯或合併這些檔案，請關閉此選項。
- 除了螢幕截圖模式之外，網頁比較在擷取網頁時不會執行其中的指令碼。因此文字、HTML 原始碼與資源樹模式看到的是伺服器傳回的 HTML，而不是經 JavaScript 處理後的畫面。螢幕截圖需要 Chromium 系列瀏覽器，且不支援 Cookie 或登入。
- 文字編輯以行為單位，並非完整的程式碼編輯器。
- 解譯器外掛程式是外部指令，而非 WinMerge 的外掛 DLL 或指令碼。
- 由機器輔助完成的翻譯尚未經過母語人士審閱。

## 授權

採用 MIT 授權條款，詳見 [LICENSE](LICENSE)。
