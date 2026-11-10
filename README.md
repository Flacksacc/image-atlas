# ImageAtlas

**Find, browse, and organize your image library while keeping the original files in place.**

ImageAtlas builds a local, searchable catalog of your photos and artwork. Use a desktop viewer, a browser gallery, or the command line to find images, review faces, explore events and trips, and identify duplicates.

The catalog stores metadata and previews in PostgreSQL. Your originals stay in their existing folders. Ratings, tags, people, and organization proposals are saved in the catalog; approving a move proposal does not move a file.

## What you can do

- **Find images:** search filenames and metadata, or enable visual search for descriptions such as “red car at night.”
- **Browse your library:** filter and sort images, compare photos, and explore folders, dates, and available GPS information.
- **Review people:** name detected faces, reuse existing identities, and correct suggested matches.
- **Organize collections:** detect suggested events, trips, and places, then edit their names and contents.
- **Keep libraries separate:** create catalogs with their own folders, people, collections, and backups.

## Requirements

The supported setup described here is **Windows with PowerShell**. Catalog passwords use Windows Credential Manager.

| Requirement | Used for |
| --- | --- |
| Python 3.12 or newer | The CLI and browser gallery; examples use Python 3.12 |
| Docker Desktop with its Linux engine running | The included PostgreSQL database with pgvector |
| Rust and the MSVC build tools, optional | Building the desktop viewer |
| Vision dependencies, optional | Visual search, content labels, and face analysis |

A GPU is optional. Models run on CPU when requested with `--cpu`; compatible GPU support can speed up analysis. First use of vision features may download model files.

## Get started

Clone the repository, open PowerShell in its folder, and start Docker Desktop.

```powershell
git clone https://github.com/Flacksacc/image-atlas.git
cd image-atlas
```

### 1. Install the CLI and start the database

```powershell
py -3.12 -m venv .venv
.\.venv\Scripts\Activate.ps1
python -m pip install -e .
python scripts/setup_local.py
docker compose up -d --wait
image-atlas db check
```

Keep using this activated environment for the commands below. If PowerShell blocks activation, use `.\.venv\Scripts\python.exe` instead of `python` and `.\.venv\Scripts\image-atlas.exe` instead of `image-atlas`.

The setup command generates a private database password in `.env`, which Git ignores. Keep that file when reusing your database.

The database runs locally on port `54329`. Docker keeps catalog data in a persistent volume, so stopping the container preserves your catalog.

### 2. Choose the folders to catalog

Replace `D:\Photos` with a folder on your computer:

```powershell
image-atlas db roots Current --root "D:\Photos"
image-atlas mine "D:\Photos" --tags metadata --limit 25
image-atlas status
```

On first use, the included local database is registered as **Current**. Assigning its folders allows image-atlas to read those originals. Folder scans include supported images in subfolders.

This first batch collects metadata, hashes, thumbnails, and previews without loading vision models. Repeat the command to process the next unfinished batch, or remove `--limit 25` to inventory the whole folder.

### 3. Open your library

The browser gallery works without building the desktop viewer:

```powershell
image-atlas gallery
```

Open the **private session link printed in the terminal**. Keep that terminal running while using the gallery; restarting it creates a new session link.

For the desktop viewer, build it once, then launch it:

```powershell
cargo build --release --manifest-path viewer/Cargo.toml
image-atlas view
```

Use the viewer's search, filters, and sorting controls to explore your catalog. Ratings, tags, and people edits affect catalog data. Opening an original in another editor lets that application access the source file.

**Click an image** to see its higher-quality database preview in a resizable side panel. **Double-click** the image or its filename to open the original in your system's default app. Remote browsing downloads the original first. If a database preview is missing, the panel shows the thumbnail and a status message; selection does not fetch the original.

## Add visual search and face analysis

Install the optional vision dependencies in the same environment:

```powershell
python -m pip install -e ".[vision]"
image-atlas mine "D:\Photos" --limit 25
image-atlas semantic-search "red car at night"
```

By default, `mine` runs metadata collection, visual embeddings, suggested content labels, and face analysis. For CPU processing, add `--cpu`. People and content labels are suggestions to review, and OCR is currently disabled.

## Everyday commands

| Task | Command |
| --- | --- |
| Update a folder and finish pending analysis | `image-atlas mine "D:\Photos"` |
| Inventory without vision models | `image-atlas mine "D:\Photos" --tags metadata` |
| Retry failed analysis | `image-atlas mine "D:\Photos" --retry-errors` |
| Search indexed text | `image-atlas search "holiday"` |
| Find images similar to an image ID | `image-atlas similar 42` |
| Find exact duplicates | `image-atlas duplicates` |
| Suggest events, trips, and places | `image-atlas collections detect` |
| Inspect catalog health and storage | `image-atlas db check` / `image-atlas db storage` |
| List running work or request cancellation | `image-atlas jobs` / `image-atlas cancel JOB_ID` |

Rerunning mining reuses completed work and updates changed files. Press **Ctrl+C** to cancel; completed records stay saved. Cloud placeholders are skipped until their contents are available locally.

Use `--prune` only with a complete folder run to mark missing files in the catalog. It does not delete originals and cannot be combined with `--limit`.

## Separate catalogs and backups

Create another catalog when you want a separate library:

```powershell
image-atlas db create Work --root "D:\Work Photos"
image-atlas --database Work mine "D:\Work Photos" --tags metadata
image-atlas --database Work view
image-atlas --database Work db backup "E:\Catalog Backups"
```

Put `--database NAME` before the command to choose a catalog for that invocation. Use `image-atlas db use Work` to change the default. Catalog folders cannot overlap, including parent and child folders belonging to different catalogs.

Backups contain catalog records and cached images, **not your original files**. Back up originals separately. Keep both the `.dump` archive and its matching `.json` manifest. Restore into a new catalog with:

```powershell
image-atlas db restore "E:\Catalog Backups\ARCHIVE.dump" "Recovered Photos"
```

A restored catalog starts read-only. Assign allowed folders before editing it or opening originals; release overlapping folders from the previous catalog first if necessary.

## Optional formats and remote access

JPEG, PNG, WebP, TIFF, BMP, GIF, AVIF, and HEIC/HEIF are supported through the installed decoders. Add camera RAW or Adobe document support as needed:

```powershell
python -m pip install -e ".[raw]"
python -m pip install -e ".[adobe]"
```

Adobe support covers Photoshop documents and PDF-compatible Illustrator previews. Documents without a usable preview can still retain searchable metadata.

Temporary remote browsing uses an SSH tunnel and a private connection code. Images are not uploaded to a hosted gallery. See the [reference guide](docs/REFERENCE.md) for remote setup, Everything discovery, advanced catalog options, and recovery controls.

## Troubleshooting

| Problem | What to do |
| --- | --- |
| Database connection fails | Start Docker Desktop, run `docker compose up -d --wait`, then `image-atlas db check`. |
| A folder is outside the allowed roots | Inspect `image-atlas db roots Current` and assign the intended folder with `--root`. Repeating `--root` supplies multiple folders; saving roots replaces the previous list. |
| Desktop viewer has not been built | Run the Cargo build command above, or use `image-atlas gallery`. |
| A preview reports that its source changed | Rerun metadata mining for that folder to update its hash, then retry the preview or analysis. |
| Some images are skipped | Make cloud files available locally and install the relevant optional decoder. Review errors with `image-atlas --json status`. |

Run `image-atlas --help` or `image-atlas COMMAND --help` for available options. For scripts, put `--json` before the command to request structured output.

## Development

```powershell
python -m pip install -e ".[test]"
python -m pytest tests/unit -q
python -m pytest -q
cargo test --manifest-path viewer/Cargo.toml
```

The Python unit tests run without PostgreSQL. The full Python suite requires the local database and uses a separate `imagecli_test` database. Several native integration tests also require explicit test database settings; see [testing details](docs/REFERENCE.md#testing). Avoid concurrent integration runs against the same test database.

The former `image-cli` command remains an alias. Existing `IMAGE_CLI_*` environment settings, catalog registry, cached models, and database volume keep their legacy storage names for compatibility. New commands and viewer binaries use the ImageAtlas name.
