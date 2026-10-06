use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};

const GRADIENT_TOP_LEFT: [f32; 3] = [30.0, 58.0, 138.0];
const GRADIENT_BOTTOM_RIGHT: [f32; 3] = [157.0, 23.0, 77.0];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Background {
    Wallpaper,
    Gradient,
    Image(PathBuf),
}

pub fn load(background: &Background, width: u32, height: u32) -> Result<Vec<u8>> {
    let path = match background {
        Background::Image(path) => Some(path.clone()),
        Background::Wallpaper => kde_wallpaper(),
        Background::Gradient => None,
    };
    let Some(path) = path else {
        eprintln!("Background: gradient");
        return Ok(gradient(width, height));
    };
    eprintln!("Background: {}", path.display());
    decode(&path, width, height)
}

fn kde_wallpaper() -> Option<PathBuf> {
    let config_dir = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| Some(PathBuf::from(env::var_os("HOME")?).join(".config")))?;
    let config =
        fs::read_to_string(config_dir.join("plasma-org.kde.plasma.desktop-appletsrc")).ok()?;

    let mut in_image_section = false;
    for line in config.lines() {
        if line.starts_with('[') {
            in_image_section = line.ends_with("[Wallpaper][org.kde.image][General]");
        } else if in_image_section && let Some(value) = line.strip_prefix("Image=") {
            let path = PathBuf::from(value.strip_prefix("file://").unwrap_or(value));
            if let Some(image) = resolve_image(&path) {
                return Some(image);
            }
        }
    }
    None
}

fn resolve_image(path: &Path) -> Option<PathBuf> {
    if path.is_file() {
        return Some(path.to_path_buf());
    }
    fs::read_dir(path.join("contents/images"))
        .ok()?
        .flatten()
        .filter_map(|entry| Some((entry.metadata().ok()?.len(), entry.path())))
        .max()
        .map(|(_, image)| image)
}

fn decode(path: &Path, width: u32, height: u32) -> Result<Vec<u8>> {
    let fill = format!(
        "scale={width}:{height}:force_original_aspect_ratio=increase:flags=lanczos,crop={width}:{height}"
    );
    let out = Command::new("ffmpeg")
        .args(["-loglevel", "error", "-i"])
        .arg(path)
        .args([
            "-vf",
            &fill,
            "-frames:v",
            "1",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgba",
            "-",
        ])
        .output()
        .context("cannot start ffmpeg (is it installed?)")?;
    let expected = width as usize * height as usize * 4;
    if !out.status.success() || out.stdout.len() != expected {
        bail!(
            "cannot read background image {}: {}",
            path.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(out.stdout)
}

fn gradient(width: u32, height: u32) -> Vec<u8> {
    let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        for x in 0..width {
            let t = (x as f32 / width as f32 + y as f32 / height as f32) / 2.0;
            for (a, b) in GRADIENT_TOP_LEFT.iter().zip(GRADIENT_BOTTOM_RIGHT) {
                rgba.push((a + (b - a) * t).round() as u8);
            }
            rgba.push(255);
        }
    }
    rgba
}
