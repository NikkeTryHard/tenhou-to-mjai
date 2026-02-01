# mjlog

mjlog XML parser.

# Usage

```
let content :: String = std::fs::read_to_string("/your/xml/path")?;

// You can read xml contains multiple mjloggm tags.
let mjlogs :: Vec<Mjlog> = parse_mjlogs(&content)?;
```

# Install

```
cargo add mjlog
```
