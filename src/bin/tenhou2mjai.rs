use convlog::tenhou::Log;
use convlog::tenhou_to_mjai;
use std::env;
use std::fs;

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
        input_path.replace(".json", ".mjai.json")
    };

    // Read input file
    let content = fs::read_to_string(input_path)?;

    // tensoul output has {"is_error": false, "log": {...}}
    // We need to extract the "log" part
    let json_value: serde_json::Value = serde_json::from_str(&content)?;
    let log_str = if let Some(log) = json_value.get("log") {
        serde_json::to_string(log)?
    } else {
        content.clone()
    };

    // Parse as tenhou Log
    let log = Log::from_json_str(&log_str)?;

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
