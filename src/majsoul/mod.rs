pub mod api;
pub mod auth;
pub mod browser;
pub mod convert;
pub mod download;
pub mod events;
pub mod gateway;
pub mod json_download;
pub mod parallel_download;
pub mod proto;
pub mod raw_download;
pub mod rpc;
pub mod tenhou_format;
pub mod tiles;
pub mod to_tenhou;
pub mod types;

pub use api::AmaeKoromoClient;
pub use convert::MajsoulConverter;
pub use download::MajsoulDownloader;
// Only re-exports with live `crate::majsoul::Name` callers are kept here;
// sibling modules import each other by module path (e.g. `super::to_tenhou::…`,
// `crate::majsoul::parallel_download::ParallelDownloader`).
pub use types::GameRecord;

