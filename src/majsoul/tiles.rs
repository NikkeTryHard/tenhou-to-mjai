//! Majsoul <-> MJAI tile conversion.

use anyhow::Result;

/// Convert Majsoul tile string (e.g., "5s", "0s", "1z") to MJAI format.
pub fn tile_str_to_mjai(s: &str) -> Result<String> {
    if s.len() < 2 {
        anyhow::bail!("Invalid tile string: {}", s);
    }

    let chars: Vec<char> = s.chars().collect();
    let num = chars[0];
    let suit = chars[1];

    let result = match suit {
        'm' | 'p' | 's' => {
            if num == '0' {
                // Red five
                format!("5{}r", suit)
            } else {
                format!("{}{}", num, suit)
            }
        }
        'z' => {
            // Honor tiles: 1-4 = winds (E/S/W/N), 5-7 = dragons (P/F/C)
            match num {
                '1' => "E".to_string(),
                '2' => "S".to_string(),
                '3' => "W".to_string(),
                '4' => "N".to_string(),
                '5' => "P".to_string(), // Haku (white dragon)
                '6' => "F".to_string(), // Hatsu (green dragon)
                '7' => "C".to_string(), // Chun (red dragon)
                _ => anyhow::bail!("Invalid honor tile: {}", s),
            }
        }
        _ => anyhow::bail!("Invalid suit: {}", suit),
    };

    Ok(result)
}

/// Convert a list of Majsoul tile strings to MJAI tile strings.
pub fn tiles_to_mjai(tiles: &[String]) -> Result<Vec<String>> {
    tiles.iter().map(|t| tile_str_to_mjai(t)).collect()
}

/// Convert MJAI tile string back to Majsoul format (for debugging).
#[allow(dead_code)]
pub fn mjai_to_tile_str(s: &str) -> Result<String> {
    let result = match s {
        "E" => "1z".to_string(),
        "S" => "2z".to_string(),
        "W" => "3z".to_string(),
        "N" => "4z".to_string(),
        "P" => "5z".to_string(),
        "F" => "6z".to_string(),
        "C" => "7z".to_string(),
        _ => {
            if s.len() < 2 {
                anyhow::bail!("Invalid MJAI tile: {}", s);
            }
            let chars: Vec<char> = s.chars().collect();
            if s.ends_with('r') && chars.len() == 3 {
                // Red five: "5mr" -> "0m"
                format!("0{}", chars[1])
            } else {
                // Regular tile: "5m" -> "5m"
                s.to_string()
            }
        }
    };
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tile_str_to_mjai() {
        // Regular tiles
        assert_eq!(tile_str_to_mjai("1m").unwrap(), "1m");
        assert_eq!(tile_str_to_mjai("9p").unwrap(), "9p");
        assert_eq!(tile_str_to_mjai("5s").unwrap(), "5s");

        // Red fives
        assert_eq!(tile_str_to_mjai("0s").unwrap(), "5sr");
        assert_eq!(tile_str_to_mjai("0p").unwrap(), "5pr");
        assert_eq!(tile_str_to_mjai("0m").unwrap(), "5mr");

        // Winds
        assert_eq!(tile_str_to_mjai("1z").unwrap(), "E");
        assert_eq!(tile_str_to_mjai("2z").unwrap(), "S");
        assert_eq!(tile_str_to_mjai("3z").unwrap(), "W");
        assert_eq!(tile_str_to_mjai("4z").unwrap(), "N");

        // Dragons
        assert_eq!(tile_str_to_mjai("5z").unwrap(), "P");
        assert_eq!(tile_str_to_mjai("6z").unwrap(), "F");
        assert_eq!(tile_str_to_mjai("7z").unwrap(), "C");
    }

    #[test]
    fn test_tiles_to_mjai() {
        let tiles = vec!["1m".to_string(), "0s".to_string(), "7z".to_string()];
        let mjai = tiles_to_mjai(&tiles).unwrap();
        assert_eq!(mjai, vec!["1m", "5sr", "C"]);
    }

    #[test]
    fn test_mjai_to_tile_str() {
        assert_eq!(mjai_to_tile_str("1m").unwrap(), "1m");
        assert_eq!(mjai_to_tile_str("5sr").unwrap(), "0s");
        assert_eq!(mjai_to_tile_str("E").unwrap(), "1z");
        assert_eq!(mjai_to_tile_str("C").unwrap(), "7z");
    }
}
