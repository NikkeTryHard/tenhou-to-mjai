use convlog::tenhou::Log;
use convlog::tenhou_to_mjai;
use std::env;
use std::fs;
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        eprintln!("Usage: {} <tenhou.json> [output.mjai.json]", args[0]);
        std::process::exit(1);
    }

    let input_path = &args[1];
    let output_path = if args.len() > 2 {
        args[2].clone()
    } else {
        let p = Path::new(input_path);
        let stem = p
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("out");
        match p.parent().filter(|p| !p.as_os_str().is_empty()) {
            Some(dir) => format!("{}/{}.mjai.json", dir.display(), stem),
            None => format!("{stem}.mjai.json"),
        }
    };
    // Read input file
    let content = fs::read_to_string(input_path)?;

    // Try the content as-is first (real tenhou.net/6 RawLog).
    let log = match Log::from_json_str(&content) {
        Ok(log) => log,
        Err(first_err) => {
            let value: serde_json::Value = match serde_json::from_str(&content) {
                Ok(v) => v,
                Err(_) => return Err(first_err.into()),
            };
            let obj = value.as_object().ok_or(
                "unrecognized input: expected tenhou.net/6 RawLog or {is_error:false,log:{...}}",
            )?;
            // Top level already looks like a RawLog: the first error stands
            // (e.g. sanma keeps NotFourPlayer instead of a generic message).
            if obj.contains_key("log") && obj.contains_key("name") && obj.contains_key("rule") {
                return Err(first_err.into());
            }
            let inner = obj.get("log").ok_or(
                "unrecognized input: expected tenhou.net/6 RawLog or {is_error:false,log:{...}}",
            )?;
            let is_error_false =
                obj.get("is_error").and_then(serde_json::Value::as_bool) == Some(false);
            let inner_has_shape =
                inner.get("name").is_some() && inner.get("rule").is_some();
            if !(is_error_false || inner_has_shape) {
                return Err(
                    "unrecognized input: expected tenhou.net/6 RawLog or {is_error:false,log:{...}}"
                        .into(),
                );
            }
            let sub = serde_json::to_string(inner)?;
            // Sanma input keeps erroring with NotFourPlayer here; no silent conversion.
            Log::from_json_str(&sub)?
        }
    };

    // Convert to MJAI events
    let events = tenhou_to_mjai(&log)?;

    // Write output - one JSON per line
    let mut output = String::new();
    for event in &events {
        output.push_str(&serde_json::to_string(event)?);
        output.push('\n');
    }

    fs::write(&output_path, output)?;

    println!("Converted {} events to {}", events.len(), output_path);

    Ok(())
}
