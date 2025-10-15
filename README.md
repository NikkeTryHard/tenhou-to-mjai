# Tenhou to MJAI

This repository provides tools and datasets for converting **Tenhou mahjong game logs** into the **MJAI** format.
It also includes preprocessed yearly datasets and Tenhou database files for AI research, data analysis, and mahjong strategy modeling.

---

## Table of Contents

* [Dataset Overview](#dataset-overview)
* [Release Structure](#release-structure)
* [File Size Notes](#file-size-notes)
* [Understanding the MJAI Format](#understanding-the-mjai-format)
* [Preparing Your Own Dataset](#preparing-your-own-dataset)
* [Licenses](#licenses)
* [Attribution](#attribution)

---

## Dataset Overview

Each release contains data for **2015–2024** in two parts per year:

| File Type | Example    | Description                                                         |
| --------- | ---------- | ------------------------------------------------------------------- |
| `.zip`    | `2024.zip` | Contains yearly folder of gzip-compressed `.mjson` game logs        |
| `.db`     | `2024.db`  | SQLite database file downloaded from Tenhou (4-player hanchan mode) |

All datasets contain only **4-player (四人麻雀)** **hanchan (半荘戦)** games.
No 3-player or tonpuu (東風戦) matches are included.

---

## Release Structure

Example structure of a yearly dataset:

```
2024.zip
 └── 2024/
     ├── 2024010100gm-00a9-0000-005a39ba.mjson
     ├── 2024010100gm-00a9-0000-00abc123.mjson
     └── ...
```

> [!NOTE]
> The `.mjson` files are **gzip-compressed** but retain the `.mjson` extension for compatibility.
> Use tools like `gzip`, `gunzip`, or `7-Zip` to decompress them.

Each release also includes a corresponding `YYYY.db` file containing Tenhou game metadata.

---

## File Size Notes

| Year      | ZIP File Size | DB File Size | Comment                             |
| --------- | ------------- | ------------ | ----------------------------------- |
| 2024      | ~1.2 GB       | ~1.54 GB     | Example year                        |
| 2021      | ~819 MB       | varies       | Example year                        |
| 2015–2024 | varies        | varies       | Each year has both `.zip` and `.db` |

> [!IMPORTANT]
> Compression ratios differ slightly by year.
> File size variations (e.g., between 2020 and 2021) are **expected** and do not affect data integrity.

> [!NOTE]
> The provided `2024.zip` and `2024.db` are **examples**.
> Each year from **2015 to 2024** includes its own `.zip` and `.db` files following the same structure.

---

## Understanding the MJAI Format

The `.mjson` files use the **MJAI protocol**, a standard for Mahjong AI communication. Each file contains a sequence of JSON objects, with one object per line, representing the events of a single game.

### Tile Notation

Tiles are represented using the following string format:

| Category          | Notation                               | Examples                               |
| ----------------- | -------------------------------------- | -------------------------------------- |
| **Manzu** (Characters) | `1m`, `2m`, ..., `9m`                  | `1m`, `5m`, `9m`                       |
| **Pinzu** (Circles)   | `1p`, `2p`, ..., `9p`                  | `1p`, `5p`, `9p`                       |
| **Souzu** (Bamboo)    | `1s`, `2s`, ..., `9s`                  | `1s`, `5s`, `9s`                       |
| **Honors**        | `E`, `S`, `W`, `N`, `P`, `F`, `C`        | East, South, West, North, White, Green, Red |
| **Red Fives**     | `5mr`, `5pr`, `5sr`                    | Red 5-Man, Red 5-Pin, Red 5-Sou      |

### Common Event Types

Each JSON object has a `type` field that describes the game event. Here are some of the most common events you will find in the logs:

*   **`start_kyoku`**: Signals the start of a new round.
    *   Provides initial state: round wind (`bakaze`), round number (`kyoku`), dealer (`oya`), dora indicator (`dora_marker`), and each player's starting hand (`tehais`).
    *   `{"type":"start_kyoku","bakaze":"E","kyoku":1,"oya":0,"dora_marker":"7s", ...}`

*   **`tsumo`**: A player draws a tile from the wall.
    *   `actor` is the player's ID (0-3). `pai` is the tile drawn.
    *   `{"type":"tsumo","actor":1,"pai":"3m"}`

*   **`dahai`**: A player discards a tile.
    *   `tsumogiri` is `true` if the discarded tile was the one just drawn.
    *   `{"type":"dahai","actor":1,"pai":"7s","tsumogiri":false}`

*   **`pon` / `chi` / `daiminkan`**: A player makes an open call from another player's discard.
    *   `actor` is the caller, `target` is the player who was called on. `pai` is the called tile, and `consumed` are the tiles from the actor's hand used to make the call.
    *   `{"type":"pon","actor":0,"target":1,"pai":"5sr","consumed":["5s","5s"]}`

*   **`ankan` / `kakan`**: A player makes a closed or added kan.
    *   `{"type":"ankan","actor":1,"consumed":["N","N","N","N"]}`

*   **`reach_accepted`**: A player's Riichi declaration is accepted.
    *   Shows the 1000-point payment and updated scores.
    *   `{"type":"reach_accepted","actor":1,"deltas":[0,-1000,0,0], ...}`

*   **`hora`**: A player wins the hand (Tsumo or Ron).
    *   Contains full win details: winning tile (`pai`), yaku (`yakus`), fu, fan, points (`hora_points`), score changes (`deltas`), and ura-dora indicators (`uradora_markers`).
    *   `{"type":"hora","actor":2,"target":2,"pai":"2m", ...}`

*   **`ryukyoku`**: The round ends in a draw.
    *   Specifies the reason (`reason`) and score changes.
    *   `{"type":"ryukyoku","reason":"fanpai", ...}`

For a complete and authoritative reference, please see the original [MJAI Protocol Documentation (in Japanese)](https://gimite.net/pukiwiki/index.php?Mjai%20%E9%BA%BB%E9%9B%80AI%E5%AF%BE%E6%88%A6%E3%82%B5%E3%83%BC%E3%83%90).

---

## Preparing Your Own Dataset

To build your own dataset from Tenhou logs, follow this multi-stage conversion process:

### 1. Download Logs Directly From Tenhou

Obtain [Tenhou logs](https://tenhou.net/sc/raw/) from official sources or your own game archive. These will be in `.xml` format.

### 2. Convert XML to JSON (Intermediate Step)

First, convert the raw Tenhou XML logs into an intermediate JSON format using a tool like [`mjlog2json`](https://github.com/d-mahjong/mjlog2json).

```bash
# This creates a directory of .json files from your .xml files
mjlog2json "C:\path\to\tenhou_xml\2024" -o "C:\path\to\tenhou_json\2024"
```

### 3. Convert JSON to MJAI Format

Next, use the `convlog` utility from the [mjai-reviewer](https://github.com/Equim-chan/mjai-reviewer) repository to convert the JSON files into the final MJAI format. This will produce **uncompressed** `.mjson` files.

```bash
# Assuming you are in the mjai-reviewer project directory
# This converts .json files into uncompressed .mjson files
cargo run --release --bin convlog -- "C:\path\to\tenhou_json\2024" "C:\path\to\tenhou_mjai\2024"
```

### 4. Compress Each MJAI File

Navigate to the output directory (e.g., `tenhou_mjai/2024`) and compress each `.mjson` file individually using `gzip`.

```bash
# This will compress each file, creating a .mjson.gz file and deleting the original
gzip *.mjson
```

> [!IMPORTANT]
> The standard for this dataset is to use the `.mjson` extension for the compressed files. After running `gzip`, you must rename the resulting `.mjson.gz` files back to `.mjson`. This can be done with a simple shell script or batch renaming tool.

### 5. Package by Year

Once the files are compressed and correctly named, create yearly zip archives.

```bash
# Example: zipping the 2024 folder
zip -r 2024.zip 2024
```

### 6. Include Corresponding DB File

Keep the Tenhou `.db` file alongside the yearly archive for metadata reference.

---

## Licenses

### Code License

This project’s code is licensed under the **Apache License 2.0**.
See [`LICENSE`](LICENSE) for details.

### Data License

The datasets are licensed under **Creative Commons Attribution 4.0 International (CC BY 4.0)**.
See [`DATA_LICENSE`](DATA_LICENSE) for details.
You may redistribute, remix, and build upon the data with proper attribution.

---

## Attribution

### Data Sources
Data is derived from:
* **Tenhou.net (天鳳)**
* **houou-logs** and other related sources

### Tooling
This dataset was prepared using the following open-source tools. We extend our gratitude to their authors and contributors.
*   **Mortal**: [https://github.com/Equim-chan/Mortal](https://github.com/Equim-chan/Mortal) - The target AI engine for the MJAI format.
*   **mjai-reviewer**: [https://github.com/Equim-chan/mjai-reviewer](https://github.com/Equim-chan/mjai-reviewer) - Specifically, the `convlog` utility for JSON to MJAI conversion.
*   **mjlog2json**: [https://github.com/tsubakisakura/mjlog2json](https://github.com/tsubakisakura/mjlog2json) - For converting raw Tenhou XML logs to JSON.

All rights to original game data remain with their respective owners. Converted datasets are redistributed for **research and educational** use under CC BY 4.0.
