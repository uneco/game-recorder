use anyhow::{bail, Context, Result};
use std::{fs, path::PathBuf};
mod mixer;
#[cfg(windows)]
mod recorder;

#[derive(Clone, Debug)]
pub struct Options {
    pub hwnd: isize,
    pub pid: u32,
    pub output: PathBuf,
    pub control: PathBuf,
}
impl Options {
    fn parse(args: impl Iterator<Item = String>) -> Result<Self> {
        let mut args = args;
        let (mut hwnd, mut pid, mut output, mut control) = (None, None, None, None);
        while let Some(key) = args.next() {
            let value = args
                .next()
                .with_context(|| format!("Missing value for {key}"))?;
            match key.as_str() {
                "--hwnd" => hwnd = Some(value.parse()?),
                "--pid" => pid = Some(value.parse()?),
                "--output" => output = Some(PathBuf::from(value)),
                "--control-dir" => control = Some(PathBuf::from(value)),
                _ => bail!("Unknown argument: {key}"),
            }
        }
        let result = Self {
            hwnd: hwnd.context("--hwnd required")?,
            pid: pid.context("--pid required")?,
            output: output.context("--output required")?,
            control: control.context("--control-dir required")?,
        };
        if result.hwnd == 0 || result.pid == 0 {
            bail!("Invalid window or process");
        }
        if !result.output.is_absolute() || !result.control.is_absolute() {
            bail!("Output and control paths must be absolute");
        }
        Ok(result)
    }
    pub fn status(&self, state: &str, message: &str) -> Result<()> {
        fs::create_dir_all(&self.control)?;
        let data = serde_json::json!({"state":state,"output":self.output,"message":message});
        let temporary = self.control.join("status.tmp");
        fs::write(&temporary, serde_json::to_vec(&data)?)?;
        // ReplaceFile/MoveFileEx keeps readers from observing a partially written JSON file.
        #[cfg(windows)]
        unsafe {
            use windows::{core::HSTRING, Win32::Storage::FileSystem::*};
            let from = HSTRING::from(temporary.as_os_str());
            let to = HSTRING::from(self.control.join("status.json").as_os_str());
            let mut attempts = 0;
            loop {
                match MoveFileExW(
                    &from,
                    &to,
                    MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
                ) {
                    Ok(()) => break,
                    Err(error)
                        if error.code() == windows::core::HRESULT::from_win32(32)
                            && attempts < 20 =>
                    {
                        // Godot may briefly have status.json open without FILE_SHARE_DELETE.
                        attempts += 1;
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                    Err(error) => return Err(error.into()),
                }
            }
        }
        #[cfg(not(windows))]
        fs::rename(temporary, self.control.join("status.json"))?;
        Ok(())
    }
}
fn main() {
    if std::env::args().nth(1).as_deref() == Some("--help") {
        println!("game-recorder --hwnd HWND --pid PID --output ABSOLUTE.mp4 --control-dir ABSOLUTE_DIRECTORY\nCreate control-dir/stop to finish. Poll status.json. Requires Windows 11 and a hardware H.264 encoder.");
        return;
    }
    let options = match Options::parse(std::env::args().skip(1)) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e:#}");
            std::process::exit(2);
        }
    };
    let result: Result<()> = options.status("starting", "").and_then(|_| {
        #[cfg(windows)]
        {
            recorder::record(&options)
        }
        #[cfg(not(windows))]
        {
            bail!("Recording is only supported on Windows");
        }
    });
    if let Err(e) = result {
        let _ = options.status("error", &format!("{e:#}"));
        eprintln!("{e:#}");
        std::process::exit(1);
    }
}

/// Fit the entire window inside 1080p, maintaining aspect ratio with even H.264 dimensions.
fn output_size(width: u32, height: u32) -> (u32, u32) {
    let scale = (1920.0 / width as f64).min(1080.0 / height as f64).min(1.0);
    (
        ((width as f64 * scale) as u32) & !1,
        ((height as f64 * scale) as u32) & !1,
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dimensions_fit_without_cropping() {
        assert_eq!(output_size(3840, 2160), (1920, 1080));
        assert_eq!(output_size(2560, 1440), (1920, 1080));
        assert_eq!(output_size(1281, 721), (1280, 720));
        assert_eq!(output_size(1000, 2000), (540, 1080));
        assert_eq!(output_size(5120, 1440), (1920, 540));
    }
}
