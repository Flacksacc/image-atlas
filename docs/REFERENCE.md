# ImageAtlas reference

[Back to the getting-started guide](../README.md)

Local image inventory and review tool. It stores searchable metadata, vectors, face assignments, and sort proposals in PostgreSQL with pgvector. It never moves or edits source images.

## Temporary remote image access

On the remote Windows computer, using the desktop account that can access the source images, run:

```powershell
image-atlas --database "Shared Photos" serve-images
```

The command prints the selected catalog and a private, temporary connection code. It listens only on localhost (port 18766 by default). Closing it stops access; it installs no persistent service or scheduled task. The visual-search text encoder loads once on demand and defaults to CUDA when available. Use `serve-images --cpu` to select CPU explicitly; an unavailable GPU falls back to CPU with a server warning.

On the client computer, run `image-atlas remote connect image-host`, paste the connection code at the hidden prompt, and keep that command open. It creates an SSH tunnel using your existing SSH alias/key and stores the session credential in Windows Credential Manager. In another terminal:

```powershell
image-atlas --json remote search "plated smores pancakes"
image-atlas --json remote search "smores" --mode text --min-pixels 2000000 --orientation landscape
image-atlas remote view
image-atlas remote preview 2277 --destination "D:\Downloads\smores-preview.jxl"
image-atlas remote download 2277 --destination "D:\Downloads\smores-original.jpg" --open
image-atlas remote disconnect
```

Search modes are `text`, `visual`, and `combined` (the default). Results include image IDs, source hashes, dimensions, original size, dates, camera information, tags, labels, and match reasons. `--limit` and `--offset` page results; `--format` filters source formats. This read-only API is pinned to one catalog and never exposes the gallery's editing routes or direct database credentials. Originals are read only on explicit download; inaccessible or cloud-only originals return a clear error instead of silently hydrating them. Existing destination files are never overwritten.

The native remote viewer displays existing WebP thumbnails first, then prefetches and decodes 1024-pixel JPEG XL previews for visible images and the next 24 results. Selection takes priority, obsolete queued work is cancelled, and stale search responses are discarded. Transfers are limited to two, decoded native caches to 256 MiB, and the client broker's content/version-keyed disk cache to 512 MiB. Close the connection command or use `remote disconnect` to stop its broker and SSH helper. On Windows, forced process termination also terminates descendants; the next connection removes stale session credentials.

## JPEG XL previews and upgrading existing images

Install/update the normal dependencies (`python -m pip install -e .`) on each computer. JPEG XL support uses pinned `imagecodecs==2026.10.10` with libjxl 0.12.0: distance 2, effort 7, two encoder threads. Previews preserve aspect ratio, EXIF orientation and transparency, convert tagged colors to sRGB, and never upscale images smaller than 1024 pixels. They are generated from original/supported RAW or document preview sources, not small thumbnails. Originals remain unchanged.

Normal `scan` and `mine` runs check metadata and derived-image versions even for unchanged, fully processed records. They fill missing or outdated previews without resetting embeddings, faces, tags, ratings, or collections. Duplicates share cached representations by source hash. Successful previews are reused; failures remain retryable; unsupported sources are recorded for the current generation version. Missing codecs remain errors rather than marking work complete.

An optional targeted backfill is available:

```powershell
image-atlas --database "Shared Photos" previews build
image-atlas --database "Shared Photos" previews build --limit 100
image-atlas --database "Shared Photos" status
image-atlas --database "Shared Photos" db storage
```

Backfill commits each result, reports progress, and supports Ctrl+C or `image-atlas cancel JOB_ID`. Reruns resume missing/failed representations. Status and storage reports include JPEG XL payload and PostgreSQL storage separately. Search and image-serving requests never generate missing previews or open originals to fill a cache. Cached thumbnails/previews remain usable when originals are unavailable. Future metadata or encoding changes must increment their corresponding generation version so ordinary rescans discover required work.

## Start

Requires Python 3.12+, Docker Desktop, and optional NVIDIA CUDA for faster models. From this directory in PowerShell:

```powershell
py -3.12 -m venv .venv
.\.venv\Scripts\python.exe -m pip install -e ".[test]"
.\.venv\Scripts\python.exe scripts/setup_local.py
docker compose up -d --wait
.\.venv\Scripts\python.exe -m pip install -e ".[vision]"
.\.venv\Scripts\image-atlas.exe db roots Current --root "D:\Images"
.\.venv\Scripts\image-atlas.exe mine "D:\Images"
.\.venv\Scripts\image-atlas.exe status
.\.venv\Scripts\image-atlas.exe search "invoice"
.\.venv\Scripts\image-atlas.exe view
.\.venv\Scripts\image-atlas.exe gallery
```

Build the native viewer once with Rust (`rustup`/`cargo` and the MSVC toolchain on Windows):

```powershell
cargo build --release --manifest-path viewer/Cargo.toml
```

If a running viewer locks the original executable on Windows, build the alternate target with `cargo build --release --bin image-atlas-viewer-updated --manifest-path viewer/Cargo.toml`. The CLI launcher chooses the newer build.


`view` opens a Rust desktop viewer with live search, a folder treemap, timeline and calendar, GPS distribution, size-versus-pixels chart, and folder-by-month heatmap. All visualizations follow the current filters; clicking a folder, date, map bin, or chart area narrows the grid. The treemap switches between image count, bytes, unique bytes within each folder, and duplicate bytes within each folder. Search runs as you type. Text matches cover indexed metadata, names, tags, people, and content labels; OpenCLIP supplies visual matches. An adjustable relevance cutoff shows the nearest results first, then completes the result set and chart counts in the background. Counts are labeled partial until complete. The grid uses cursor paging and fetches visible stored thumbnails. Selecting an image opens its higher-quality stored preview in a resizable side panel. Double-clicking opens the original in the system default app; remote originals are downloaded first. The viewer cancels superseded database queries and reports its UI-frame p95 timing.

Right-click a thumbnail, table row, or full image to browse its folder inside the viewer, reveal the file in Explorer, open its folder, jump to its date, find similar images or duplicates, compare it, tag or rate it, open the original, or create a reviewed move proposal. **Open with** uses Windows; add favorite editor executable paths in the viewer to launch Photoshop or other editors on the original file. The viewer also has details, filmstrip, and exact duplicate stack modes, a contact sheet for selected images, saved searches, and a two-to-four-image comparison window. Select a duplicate stack to preview its representative image; right-click and choose **Show all exact copies** to expand it. Source files are never moved by these actions. Missing originals can still show stored thumbnails. The People tab assigns detected face groups to a new name or an existing person ID, and can ignore a person; the Sort proposals tab approves or rejects proposals. Set `IMAGE_CLI_VIEWER_BIN` for a separately located viewer binary.

The **Sort images** icon toolbar is always visible above the results. Click an icon to sort by match, date, name, size, pixels, folder, camera, rating, face count, duplicate count, or stable shuffle; click the active icon again to reverse it. The **Order** and **then** menus expose all 21 sort fields, with independent directions. Details-view column headers also sort on click. Sorting covers the complete filtered result set, including pages not loaded yet, and works in duplicate-stack view.

**Group by** organizes images into visible sections by confirmed people, face sample sets, day, month, year, folder, camera, format, rating, review state, face count, orientation, resolution, or file-size bucket. Images containing several people are grouped by their combination of person IDs and appear once. Images within each section use the selected sort order. Saved searches retain sorting and grouping; older saved searches remain readable. Hover report marks, legends, controls, face samples, image properties, and proposal rows for explanations and underlying values. Grouping and sorting do not move files.

If a running viewer locks the normal executable, a newer `image-atlas-viewer-updated.exe` in the release directory is selected automatically by `image-atlas view`. An explicit `IMAGE_CLI_VIEWER_BIN` still takes precedence.

The native viewer's top **Database** picker includes registered image catalogs
and **Assets — Asset library** from the sibling asset-cli project. Launching with
either `image-atlas view` or `asset-cli view` opens the same application; switch
databases in that window to use the tools for the selected catalog. Image and
asset views retain their state when switching between them. Selecting a different
image database clears image-specific caches as before. Asset retrieval can
continue while viewing images. **Refresh databases** reloads registered image
catalogs. The asset connection uses `ASSET_CLI_VIEWER_DSN` or `ASSET_CLI_DSN` when
set, otherwise the default local asset database; schemas and credentials stay
separate, and choosing a database does not change the CLI default.

**Database details** in the shared header opens a simple editor for the current
database's display name, description, and notes. Changes persist locally and the
picker reflects the saved name. Image names also update the image-atlas registry;
database identity, connection credentials, folder permissions, and stored records
keep their existing settings. The same dialog shows PostgreSQL database size,
table and index storage, host, database name, version, and table row estimates.
Size inspection runs in the background using a read-only transaction. Display
metadata can still be edited when size inspection is unavailable. Database bytes
include stored previews and indexes; original images and downloaded asset packs
are separate files.

In asset mode, **Locally stored** and the drive icons include configured LAN
storage as well as files on this computer. Published SSH archives count from
their archive records, including when their host is offline; fetching verifies
the connection and file contents. Shared-folder and mapped-drive files are
checked in the background. Browsing SSH archive records does not mount a drive
or download source files.

The web gallery also has an icon sorting toolbar, primary/secondary ordering, grouping by the same properties, hover details on image/face/proposal reports, and **Load more** paging in the chosen order.

**Collections** in both viewers browses persistent Events, Trips, and Places. Click **Detect collections** after mining, or run `image-atlas collections detect`. Detection uses indexed data only: it never opens originals, hydrates cloud placeholders, or sends images online. It groups GPS observations into bounded places, capture-time sessions into events, and chronological visits into photographed journeys, including day outings. Occasion labels are suggestions from existing content labels. Missing-date photos need two known people plus strong visual similarity to join an event; unsupported photos stay unclassified. Confidence is a heuristic, not a calibrated probability.

Edit names inline, confirm or dismiss suggestions, select images to remove or split, add catalog image IDs, or merge another collection of the same kind. **Browse images** retains full sorting and pagination. Stable IDs and user names survive redetection; manually edited membership boundaries are preserved, and dismissed suggestions remain suppressed. Trips expose linked events and ordered location visits, including return visits, in a local coordinate plot. Lines connect photographed stops, not roads. Event/Trip/Place grouping and collection-name search are available in both viewers. Changed metadata or analysis marks related collections as needing refresh.

Detection defaults: 250 m place radius (maximum cluster diameter 500 m), two-hour event gaps or five-kilometre changes, and journeys spanning at least ten kilometres across two places, separated after 72-hour gaps or 21 days. Capture offsets are used where available; unknown clock offsets are grouped separately and do not establish precise travel durations. File modification times do not establish events or trips. These are editable suggestions, rather than complete travel histories. Optional `--root PATH` limits detection to one indexed root and keeps that scope's collections separate from whole-catalog collections.

```powershell
.\.venv\Scripts\image-atlas.exe collections detect
.\.venv\Scripts\image-atlas.exe collections list --kind event
.\.venv\Scripts\image-atlas.exe collections show 12
.\.venv\Scripts\image-atlas.exe collections review 12 rename --name "Our beach outing"
.\.venv\Scripts\image-atlas.exe collections review 12 add --image-id 34 --image-id 35
.\.venv\Scripts\image-atlas.exe collections review 12 split --image-id 34 --name "Dinner"
.\.venv\Scripts\image-atlas.exe collections review 12 merge --other-id 15
.\.venv\Scripts\image-atlas.exe collections review 12 dismiss
.\.venv\Scripts\image-atlas.exe collections settings --values '{"trip_distance_km": 50}'
```

**Look up this place** (or `collections lookup ID`) explicitly sends one selected coordinate to OpenStreetMap Nominatim for an editable place name. Ordinary detection stays offline; bulk naming is disabled. Results are cached, requests are serialized and spaced at least 1.1 seconds apart, the application identifies itself, and the provider URL can be changed through collection settings. © OpenStreetMap contributors. Public-service use follows the [Nominatim usage policy](https://operations.osmfoundation.org/policies/nominatim/): no heavy or systematic use, at most one request per second, and no confidential coordinates. Reverse lookup can return a nearby object's address, so review suggested names.

High-resolution images above Pillow's warning threshold are counted as `large_images` in the metadata summary. Pillow's hard pixel-size guard remains enabled; oversized images are skipped with a scan error. Cancellation retains completed metadata counts and committed image records. Rerun mining to reuse completed work and finish pending stages.

The web gallery remains at `http://127.0.0.1:18765`. The included database binds only to localhost on port 54329. Change the compose password before exposing it to other machines. Mining scans a single image or supported images under an allowed folder, including subfolders. It never moves originals.

## Separate image catalogs

**Databases** in the native viewer and web gallery creates and registers catalogs, edits allowed folders, sets the CLI default, inspects health/storage, and backs up or restores data. The selector changes the catalog for that window or browser tab. Switching clears images, previews, selections, people, and collection caches. Changing the CLI default affects future commands; running jobs keep their selected database. Restart viewers and gallery servers opened before this feature was installed.

Each catalog is a separate PostgreSQL database with a stable UUID. Your original local database is adopted as **Current**, preserving existing records and inferring allowed folders from indexed roots. New installations assign folders before their first mining run. Existing mixed catalogs are retained intact; creating another catalog does not split or move records.

```powershell
.\.venv\Scripts\image-atlas.exe db list
.\.venv\Scripts\image-atlas.exe db create Personal --root "D:\Personal Photos"
.\.venv\Scripts\image-atlas.exe db create Work --root "D:\Work Photos"
.\.venv\Scripts\image-atlas.exe db use Personal
.\.venv\Scripts\image-atlas.exe --database Work mine "D:\Work Photos"
.\.venv\Scripts\image-atlas.exe --database Work view
.\.venv\Scripts\image-atlas.exe db roots Work --root "D:\Work Photos" --root "E:\Work Archive"
.\.venv\Scripts\image-atlas.exe --database Work db backup "E:\Catalog Backups"
.\.venv\Scripts\image-atlas.exe db restore "E:\Catalog Backups\ARCHIVE.dump" "Recovered Work"
```

`--database NAME_OR_UUID` precedes the command. Resolution order is explicit selection, `IMAGE_CLI_DSN`, the saved CLI default, then the original local connection. An unregistered environment connection can be inspected but cannot mine images until registered with `db register NAME [--dsn CONNECTION] [--root FOLDER]`. Registration without explicit roots infers indexed folders; unrelated databases are rejected. Passwords are stored in Windows Credential Manager; the versioned, locked registry at `%LOCALAPPDATA%\image-cli\catalogs.json` contains credential references. `IMAGE_CLI_CONFIG_DIR` can isolate a registry for tests. Catalog separation uses the existing PostgreSQL role, rather than a separate server or role per catalog.

Owned folders cannot overlap another catalog, including nested folders and Windows aliases. Processing checks resolved paths before original-file access; Everything searches are restricted to owned folders even with `--include-system`. Editing folders is blocked during affected jobs. Removing a folder preserves its indexed metadata and cached thumbnails, but prevents further original-file access. `db roots NAME --clear` releases all folders and makes the catalog read-only.

Backups contain a PostgreSQL custom archive and adjacent JSON manifest with checksum, identity, roots, schema, server, and extension versions. They preserve thumbnails, vectors, people/ignored identities, proposals, saved views, and manual collection edits. They contain neither original images nor connection passwords. Backups use a consistent database snapshot and may run while mining. Compatible `pg_dump`/`pg_restore` on PATH are used when present; this project's existing local Docker database supplies them otherwise. Binary data is streamed directly, without a PowerShell text pipeline. Job progress reports elapsed time and archive bytes transferred; use the manager's Jobs/Cancel controls or `--database NAME jobs` and `--database NAME cancel ID`. Incomplete backups are not published.

Restore creates a new database and UUID; it never overwrites the source. Restored catalogs start read-only with no owned folders. Release folders from the old catalog before assigning them to the restored catalog, or assign different folders. Both the `.dump` and matching `.json` manifest must be available. Corrupt or incompatible restores are rejected; failed operations clean up only their newly created target. Database deletion and automatic backup schedules are not included.

Web data URLs use `/api/catalogs/{UUID}/…`; unscoped data requests are rejected once catalogs are registered. Native database connections, Python helpers, and jobs are pinned to their catalog. Vision workers do not inherit database credentials.

## Mine options

`mine` always collects file properties, EXIF and generation metadata, hashes, camera/GPS fields, color and technical-quality summaries, and a small persistent thumbnail. By default it also runs OpenCLIP embeddings, compact OpenCLIP content labels, and face detection/grouping. Use `--tags` with comma-separated `metadata`, `embeddings`, `labels`, and `faces` to choose analysis stages; `metadata` alone does the first inventory. Labels are suggestions based on the image vector; they are not definitive object recognition. OCR remains disabled.

```powershell
.\.venv\Scripts\image-atlas.exe mine "D:\Images\photo.png" --tags metadata
.\.venv\Scripts\image-atlas.exe mine "D:\Images" --tags embeddings,faces
.\.venv\Scripts\image-atlas.exe mine "D:\Images" --tags embeddings,labels
.\.venv\Scripts\image-atlas.exe mine "D:\Images" --limit 25
.\.venv\Scripts\image-atlas.exe mine "D:\Images" --retry-errors --prune
.\.venv\Scripts\image-atlas.exe mine "D:\Images" --json
.\.venv\Scripts\image-atlas.exe mine searcheverything --limit 25
.\.venv\Scripts\image-atlas.exe mine searcheverything --limit 25 --exclude-dir "D:\Program Art" --exclude-dir "E:\Cache"
.\.venv\Scripts\image-atlas.exe jobs
.\.venv\Scripts\image-atlas.exe cancel
.\.venv\Scripts\image-atlas.exe faces groups
.\.venv\Scripts\image-atlas.exe faces tag 12 "Alice"
.\.venv\Scripts\image-atlas.exe faces people
.\.venv\Scripts\image-atlas.exe faces tag 19 --person-id 3
.\.venv\Scripts\image-atlas.exe faces ignore 27
.\.venv\Scripts\image-atlas.exe semantic-search "red car at night"
.\.venv\Scripts\image-atlas.exe similar 42
.\.venv\Scripts\image-atlas.exe duplicates
.\.venv\Scripts\image-atlas.exe duplicates --near
```

`--limit 25` selects at most 25 images that still need the requested work. Only those images are indexed and passed through every selected analysis stage. The command may inspect directory entries and file timestamps to find the next batch, but it does not decode or hash other images. Rerunning it advances to the next unfinished batch. The displayed picture count is the selected batch size, and JSON also reports how many image paths were examined to find it. `--retry-errors` includes failed analysis. `--prune` marks missing files under a fully scanned root; it cannot be combined with `--limit`, because a partial run cannot establish which files are missing. A failed stage appears in the JSON summary and the command exits nonzero after trying other selected stages. Individual image failures are recorded and can be retried.

In an interactive console, `mine` updates one status line instead of printing a line for every picture. It shows stage progress, selected picture count, **images per second averaged over only the last five seconds**, CPU, RAM, and VRAM. At the start of a stage, throughput uses the available elapsed time until five seconds have passed; old completions expire, so the rate drops to zero after five idle seconds. Each stage starts a fresh window. CPU is the CLI plus its model worker processes as a percentage of all logical CPU capacity; RAM is their resident memory. VRAM is **device-wide NVIDIA GPU memory used/total**, including other applications, because per-process VRAM is not reliably available on this Windows setup. The final line shows the last five-second throughput of the most recent stage that processed images and peak measured CPU, RAM, and device-wide VRAM. Short peaks can fall between the 0.75-second resource samples. Reports are readable text by default, including redirected output, errors, and cancellation. `mine --json` prints structured results; all commands also accept the global flag before the command, such as `image-atlas --json status`. Automation and native-viewer helpers explicitly request JSON. Stage and final throughput use `images_per_second` and `throughput_window_seconds`; final metrics also identify `throughput_stage` and the actual `images_processed` during scanning, including unchanged files and failed attempts. Selected but unprocessed images do not inflate throughput on cancellation.

The console now shows a completion bar and count for discovery, scanning, embeddings, and faces. A complete run counts eligible image paths before mining so the scan bar has a known total; a limited run shows selection against `--limit`, then the exact selected count. The initial `COUNT` phase measures the total and cannot show a final number until enumeration finishes. Mining skips Windows cloud placeholders marked offline or recall-on-access; they can be mined on a later run once their data is local. Google Drive streamed files are checked individually, just like mirrored files and OneDrive files: downloaded files are eligible even on a virtual drive, while files marked offline or recall-on-access are skipped. Pinning policy alone does not determine availability, and each stage rechecks the file state so downloads and cache eviction are respected. The JSON summary reports `skipped_cloud`. Use `--exclude-dir` to exclude additional folders when using `searcheverything`.

Embedding uses OpenCLIP ViT-B-32 (512 dimensions, stored as half precision). Face detection/recognition uses OpenCV YuNet and SFace; their model files download to `%USERPROFILE%\.cache\image-cli` on first use. Face groups are suggestions. People keep stable database IDs. Tagging a group assigns its *current* faces to a new name or a selected existing ID; new faces matching a named person remain suggestions for review. **Ignore person** gives an unnamed sample group its own ignored identity, or marks its assigned person ignored. All assigned groups and suggestions for that identity disappear from tagging, and future confident matches are automatically assigned to the ignored identity. Low-confidence matches still require review. Ignoring retains recognition samples and does not affect source images. The web gallery offers the same controls. The `--near` duplicate search uses a difference-hash prefix prefilter and is not exhaustive.

The default install includes `pillow-heif` for HEIC/HEIF support. For DNG, CR2, NEF, ARW, RAF, RW2, ORF, and PEF, install `pip install -e ".[raw]"` first. RAW thumbnails and indexing use embedded previews where available; opening an original in the viewer uses RAW decoding. Unsupported or corrupt files are reported as scan errors and the scan continues.

For Photoshop `.psd`/`.psb` and Illustrator `.ai`, install `pip install -e ".[adobe]"` in the CLI environment. Folder scans and Everything include these formats. Photoshop uses its saved composite when available, with layer compositing as a fallback; unsupported effects may look different. Metadata includes canvas dimensions, bit depth, color mode, layer names/types/visibility (first 2,000), and embedded XMP where available. Illustrator uses PDF-compatible artwork for previews; open the original in its default application to browse other artboards in multipage documents. The first artboard supplies the catalog thumbnail and visual embedding. Illustrator sizes are document points, identified in document metadata.

An Illustrator file without PDF-compatible content is still indexed: path, size, modification time, SHA-256, available DSC fields (title, creator, dates, bounds, resources), and embedded XMP in its header/trailer are retained and searchable. Opaque native data cannot be interpreted as artwork; dimensions or other fields that cannot be extracted remain unknown. No thumbnail is created, and embedding/face/label stages are marked `skipped`, with the reason in Image properties under document metadata. This is a successful metadata-only scan and unchanged files are not repeatedly selected. Saving a changed, PDF-compatible version automatically enables thumbnails and visual processing on the next scan. Adobe documents with missing decoders, invalid content, or oversized previews also retain their obtainable metadata and a preview reason.

Adobe rendering runs in isolated workers with a 25-second deadline, a monitored 2 GiB RSS ceiling, a 40-megapixel Photoshop canvas limit, and a 2,048-pixel preview edge. Successful and unavailable previews are cached by source path/size/modification time and artboard. Set `IMAGE_CLI_DOCUMENT_CACHE` to relocate this derived cache; its files are excluded from discovery. Defaults are `%LOCALAPPDATA%/image-cli/document-previews` on Windows. Source documents are never rewritten. Shared CUDA environments do not need Adobe decoders: the CLI sends a temporary raster to the embedding worker. Photoshop and Illustrator are not required for supported previews; Windows Open/Open with still opens the original in its associated editor.

From the repository root, run `python scripts/bench_catalog.py --seed --rows 1000000` to generate and measure a separate `imagecli_bench` database. It never seeds the normal catalog. The ignored Rust test `benchmark_million_image_explore` measures the actual viewer page and facet path when `IMAGE_CLI_BENCH_DSN` points to that benchmark database.

To reuse a compatible environment that already has Torch and OpenCLIP, set `$env:IMAGE_CLI_VISION_PYTHON='PATH\TO\python.exe'` if needed. Face detection uses OpenCV in the image-atlas environment.

The vision worker's image loader has no database dependencies: `psycopg` is needed only in the CLI environment. A readiness check loads the worker before processing the backlog; setup failures stop the embedding stage with one error and leave pending images available for retry. Individual corrupt images still get separate errors. If the shared worker lacks HEIF or RAW decoders, the CLI uses its local decoder and sends a temporary lossless PNG to the worker, then deletes it. Originals are unchanged and the shared environment needs no extra decoder or database packages.

`searcheverything` in the source position discovers supported image paths through the running [Everything desktop client's Windows IPC index](https://www.voidtools.com/support/everything/sdk/). It searches only the selected catalog's allowed folders indexed by Everything, then mines each discovered image as a folder run would. No separate `es.exe` installation is needed. Use `--limit` to process batches; a complete index can contain hundreds of thousands of images. Existing images keep their original folder association. `--prune` is unavailable for this global source because an index result cannot establish that an original file was deleted. A mining command runs in the foreground: press Ctrl+C to stop, or run `image-atlas jobs` in another terminal and `image-atlas cancel [JOB_ID]`. The ID is optional when one job is running. Completed images stay committed; cancellation returns exit code 130 and a JSON summary. Cancellation during a single image decode or model call takes effect at the next checkpoint.

Everything discovery skips Windows, Program Files, ProgramData, WindowsApps, recycle and system volume folders, user AppData, and common dependency/build folders (`.git`, `.venv`, `node_modules`, `__pycache__`) by default. These defaults apply only to `searcheverything`; a file or folder source scans exactly what you specify. Repeat `--exclude-dir PATH` for additional directories. Use `--include-system` to turn off the defaults while keeping your explicit `--exclude-dir` entries. Exclusions filter discovery only; previously indexed records remain in the database.

Face inference runs in an isolated subprocess in the CLI environment, without database credentials. OpenCV receives an EXIF-oriented image bounded to a 2048-pixel longest edge; bounding boxes are mapped back to original oriented coordinates. JPEG decoding uses a reduced-resolution draft when supported. Very small faces in large images may be missed at this resolution. The worker is recycled after 200 successful images to release native caches. A native worker crash or a 120-second image timeout marks that image as an error, restarts the worker for the next image, and keeps already committed face assignments. Setup failures leave unprocessed images pending. Cooperative cancellation also works while waiting on the worker. Jobs record the active image path for diagnosis, and cancellation summaries retain completed face counts. Rerun `mine PATH --tags faces` to resume pending work; add `--retry-errors` to retry failed images.

## Agent commands

Commands print readable reports by default. Automation must opt in with the global `--json` flag:

```powershell
image-atlas --json show 42
image-atlas --json search "battle"
image-atlas --json faces groups
image-atlas --json sort propose 42 "D:\Sorted\Game Art\sword.png" "Game concept art with sword"
image-atlas --json sort list
image-atlas --json sort review 1 approved
```

`sort propose` records a destination and reason in the database. `sort review` (or the gallery buttons) marks a proposal approved or rejected. No command moves an original. The gallery shows the stored proposals and face groups.

## Data and sizing

The database stores paths, derived information, and one small thumbnail per distinct image hash; it does not store original images. Thumbnails are EXIF-oriented WebP files, scaled to fit within 160 × 160 pixels at quality 45. They normally take a few kilobytes each, though complex images and database overhead vary. Exact duplicate files share thumbnail bytes. Gallery previews remain available after originals go missing: select **Show missing files**. Rerun `image-atlas mine PATH` to backfill thumbnails for previously indexed images. When scanning a new directory, the CLI recognizes an exact-content move if one previously indexed path with the same SHA-256 no longer exists. It keeps that image's embedding, face tags, and sort proposals. A copy whose old path still exists gets its own image record; ambiguous moves also get a new record. `image-atlas status` reports thumbnail counts and raw thumbnail bytes. Each 512-dimensional half-precision image vector uses about 1 KiB before row and index overhead. Rescanning a changed file invalidates its embedding and face results. Exact duplicates use SHA-256; candidate near-duplicates use a 64-bit difference hash. Existing OCR values remain stored in the database but are no longer processed, searched, or shown.

Check the database and see where its disk space goes:

```powershell
.\.venv\Scripts\image-atlas.exe db check
.\.venv\Scripts\image-atlas.exe db storage
```

`db check` reports connection health, PostgreSQL and extension versions, required tables, and the image count. `db storage` reports total database bytes, each app table's heap, index, and other bytes (including TOAST storage), plus raw thumbnail, vector, and text payload bytes. Raw payloads are already included in table totals; do not add them again. Database size also includes PostgreSQL catalogs and extension objects. Both commands are read-only and print readable text; use `image-atlas --json db check` or `image-atlas --json db storage` for structured output.

## Viewer review and recovery

People → **Review all faces** pages through every group member and can reassign, split, or remove selected false detections. **Manage identities** renames or merges stable person IDs and restores ignored people. Read-only catalogs disable editing actions. Image selection toggles individually; contact sheets can export up to 1000 selected images to a new PNG without reading originals or overwriting an existing file. Collections can add the current image selection and offer named merge targets. The proposals queue now has status/text filters, image inspection, and stable pagination.

Failed previews stop retrying automatically; use **Retry preview** or the image menu's **Retry thumbnail / preview** after correcting the source or decoder. Filmstrip supports keyboard navigation and loads beyond the first page. Refreshes retain the displayed image by ID, while content-hash changes invalidate cached pixels. Visual-search model loading is deferred until a text-content search needs it.

Person names are labels, so different people can share a name. Choose an existing **person ID** to reuse an identity; **New person** creates a distinct ID. Saved person filters store IDs and follow identity merges. Older writable catalogs are upgraded on opening; read-only archives display corrected EXIF-oriented dimensions without being rewritten. Reopen older viewer windows after upgrading the program.

**Reconnect** restores the current database connection and search after an outage. **Retry visual search** restarts a failed model; model startup and requests have bounded deadlines. Identity merges and face removal require confirmation. **Undo removed detections** restores the last removal and its assignments until the window closes or switches databases. Selection has visible marks, comparison panes scroll independently, and collection members load in pages of 100.

`image-atlas gallery` prints a private session link. Open that link to authenticate; restarting the gallery expires it. Database management and thumbnails require the session, and untrusted Host/Origin requests are rejected. Loopback remains the default. Network binding requires `--tls-cert` and `--tls-key`; use `--allowed-host NAME` for an explicitly allowed hostname when binding a wildcard address. Registration supports `--dsn-stdin` for tools that must avoid putting passwords in process arguments.


## Preview source verification

Before publishing a new preview, image-atlas checks that the source still matches its indexed SHA-256, both before and after encoding. A changed source produces a retryable error and no preview bytes are saved under the old hash. Run `image-atlas mine "D:\Photos" --tags metadata` to update its catalog record, then rerun the desired analysis stages. Existing successful cached previews remain available without opening originals.

## Testing

From the repository root with the Python environment active:

```powershell
python -m pip install -e ".[test]"
python -m pytest tests/unit -q
```

Unit tests isolate local configuration and run without PostgreSQL. The full Python suite needs the database from `compose.yaml` on port 54329. It creates and resets a dedicated `imagecli_test` database; run it separately from native integration tests:

```powershell
docker compose up -d --wait
$env:PGCONNECT_TIMEOUT = '5'
python -m pytest -q
```

Run the native test suite with Cargo:

```powershell
cargo test --manifest-path viewer/Cargo.toml
```

Several native tests return early unless `IMAGE_CLI_TEST_DSN` is set. To exercise those image-database paths, first complete the Python suite so the test schema exists, then run:

```powershell
$env:IMAGE_CLI_TEST_DSN = python -c "from image_atlas.db import DEFAULT_DSN; print(DEFAULT_DSN.rsplit('/', 1)[0] + '/imagecli_test')"
cargo test --manifest-path viewer/Cargo.toml -- --test-threads=1
Remove-Item Env:IMAGE_CLI_TEST_DSN
```

Native asset integration tests additionally require the sibling asset-cli project's dedicated test database via `ASSET_CLI_TEST_DSN`. The million-image benchmark is intentionally ignored unless its separate benchmark catalog is prepared. A successful default Cargo run alone does not establish database integration coverage.

Local audit reports, runtime fixtures, pytest scratch data, database dumps, and build products are ignored by Git. Keep source code, JavaScript package data, tests, documentation, and `viewer/Cargo.lock` in the published repository.
