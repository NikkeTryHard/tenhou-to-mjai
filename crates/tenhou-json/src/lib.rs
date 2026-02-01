//! # tenhou-json
//!
//! Tenhou JSON parser.
//!
//! # Usage
//!
//! ```
//! let content :: String = std::fs::read_to_string("/your/json/path")?;
//! let tenhou_json :: TenhouJson = parse_tenhou_json(&content)?;
//! ```
//!
//! # Install
//!
//! ```
//! cargo add tenhou-json
//! ```

pub mod score;
#[rustfmt::skip]
pub mod calc;
pub mod exporter;
pub mod model;
pub mod parser;
