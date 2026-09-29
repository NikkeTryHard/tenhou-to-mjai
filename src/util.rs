use anyhow::{Context, Result};
use indicatif::{ProgressBar, ProgressStyle};
use std::fs;
use std::path::{Path, PathBuf};

pub fn progress_bar(len: u64) -> Result<ProgressBar> {
    progress_bar_with(
        len,
        "{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({eta})",
        "#>-",
    )
}

pub fn progress_bar_with(len: u64, template: &str, chars: &str) -> Result<ProgressBar> {
    let pb = ProgressBar::new(len);
    pb.set_style(
        ProgressStyle::default_bar()
            .template(template)?
            .progress_chars(chars),
    );
    Ok(pb)
}

pub fn http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36")
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .context("Failed to build HTTP client")
}

/// Write via `{out}.tmp.{pid}` then rename. On any write failure the temp is
/// removed and no output file appears; a rename failure also cleans up.
/// Shared with `export` (same atomic-output contract).
pub(crate) fn write_atomic(output_path: &Path, write_tmp: impl FnOnce(&Path) -> Result<()>) -> Result<()> {
    let tmp_path = PathBuf::from(format!(
        "{}.tmp.{}",
        output_path.display(),
        std::process::id()
    ));
    match write_tmp(&tmp_path) {
        Ok(()) => {
            if let Err(e) = fs::rename(&tmp_path, output_path) {
                let _ = fs::remove_file(&tmp_path);
                return Err(e.into());
            }
            Ok(())
        }
        Err(e) => {
            let _ = fs::remove_file(&tmp_path);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "tenhou_convert_{}_{}_{}",
            name,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn atomic_write_success_leaves_no_tmp() {
        let dir = unique_test_dir("ok");
        let out = dir.join("x.mjson.gz");
        let tmp = PathBuf::from(format!(
            "{}.tmp.{}",
            out.display(),
            std::process::id()
        ));

        write_atomic(&out, |tmp| {
            fs::write(tmp, b"hello")?;
            Ok(())
        })
        .unwrap();

        assert_eq!(fs::read(&out).unwrap(), b"hello");
        assert!(!tmp.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn atomic_write_failure_leaves_no_output() {
        let dir = unique_test_dir("fail");
        let out = dir.join("x.mjson.gz");
        let tmp = PathBuf::from(format!(
            "{}.tmp.{}",
            out.display(),
            std::process::id()
        ));

        let res: Result<()> = write_atomic(&out, |tmp| {
            fs::write(tmp, b"partial")?;
            anyhow::bail!("injected failure");
        });

        assert!(res.is_err());
        assert!(!out.exists());
        assert!(!tmp.exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
