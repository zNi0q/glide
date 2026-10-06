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
        Background::Wallpaper => desktop_wallpaper(),
        Background::Gradient => None,
    };
    let Some(path) = path else {
        eprintln!("Background: gradient");
        return Ok(gradient(width, height));
    };
    eprintln!("Background: {}", path.display());
    decode(&path, width, height)
}

pub fn path_from_uri(value: &str) -> PathBuf {
    PathBuf::from(percent_decode(
        value.strip_prefix("file://").unwrap_or(value),
    ))
}

fn desktop_wallpaper() -> Option<PathBuf> {
    let desktop = env::var("XDG_CURRENT_DESKTOP")
        .unwrap_or_default()
        .to_lowercase();
    let is = |names: &[&str]| desktop.split(':').any(|d| names.contains(&d));
    let detected = if is(&["kde"]) {
        kde()
    } else if is(&["gnome", "ubuntu", "unity", "budgie", "pantheon"]) {
        gnome()
    } else if is(&["x-cinnamon", "cinnamon"]) {
        gsettings_image("org.cinnamon.desktop.background", "picture-uri")
    } else if is(&["mate"]) {
        gsettings_image("org.mate.background", "picture-filename")
    } else if is(&["xfce"]) {
        xfce()
    } else if is(&["cosmic"]) {
        cosmic()
    } else {
        None
    };
    detected
        .or_else(|| {
            image_from(
                output_of("hyprctl", &["hyprpaper", "listactive"])
                    .as_deref()
                    .and_then(parse_hyprpaper),
            )
        })
        .or_else(|| {
            image_from(
                output_of("swww", &["query"])
                    .as_deref()
                    .and_then(parse_swww),
            )
        })
        .or_else(|| {
            image_from(
                output_of("pgrep", &["-a", "swaybg"])
                    .as_deref()
                    .and_then(parse_swaybg),
            )
        })
}

fn image_from(path: Option<&str>) -> Option<PathBuf> {
    resolve_image(&path_from_uri(path?))
}

fn output_of(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (out.status.success() && !text.is_empty()).then_some(text)
}

fn config_dir() -> Option<PathBuf> {
    env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| Some(PathBuf::from(env::var_os("HOME")?).join(".config")))
}

fn gnome() -> Option<PathBuf> {
    let dark = output_of(
        "gsettings",
        &["get", "org.gnome.desktop.interface", "color-scheme"],
    )
    .is_some_and(|scheme| scheme.contains("dark"));
    let keys = if dark {
        ["picture-uri-dark", "picture-uri"]
    } else {
        ["picture-uri", "picture-uri-dark"]
    };
    keys.iter()
        .find_map(|key| gsettings_image("org.gnome.desktop.background", key))
}

fn gsettings_image(schema: &str, key: &str) -> Option<PathBuf> {
    let value = output_of("gsettings", &["get", schema, key])?;
    image_from(Some(value.trim_matches('\'')))
}

fn xfce() -> Option<PathBuf> {
    let properties = output_of("xfconf-query", &["-c", "xfce4-desktop", "-l"])?;
    properties
        .lines()
        .filter(|property| property.ends_with("/last-image"))
        .find_map(|property| {
            image_from(
                output_of("xfconf-query", &["-c", "xfce4-desktop", "-p", property]).as_deref(),
            )
        })
}

fn cosmic() -> Option<PathBuf> {
    let config =
        fs::read_to_string(config_dir()?.join("cosmic/com.system76.CosmicBackground/v1/all"))
            .ok()?;
    image_from(parse_cosmic(&config))
}

fn parse_cosmic(config: &str) -> Option<&str> {
    let start = config.find("Path(\"")? + "Path(\"".len();
    let len = config[start..].find('"')?;
    Some(&config[start..start + len])
}

fn parse_hyprpaper(output: &str) -> Option<&str> {
    output
        .lines()
        .find_map(|line| line.split_once(" = ").map(|(_, path)| path.trim()))
}

fn parse_swww(output: &str) -> Option<&str> {
    output
        .lines()
        .find_map(|line| line.split_once("image: ").map(|(_, path)| path.trim()))
}

fn parse_swaybg(output: &str) -> Option<&str> {
    output.lines().find_map(|line| {
        let mut args = line.split_whitespace();
        args.find(|arg| *arg == "-i" || *arg == "--image")?;
        args.next()
    })
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes
            .get(i + 1..i + 3)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (bytes[i], hex) {
            (b'%', Some(byte)) => {
                decoded.push(byte);
                i += 3;
            }
            (byte, _) => {
                decoded.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

fn kde() -> Option<PathBuf> {
    let config =
        fs::read_to_string(config_dir()?.join("plasma-org.kde.plasma.desktop-appletsrc")).ok()?;

    let mut in_image_section = false;
    for line in config.lines() {
        if line.starts_with('[') {
            in_image_section = line.ends_with("[Wallpaper][org.kde.image][General]");
        } else if in_image_section
            && let Some(value) = line.strip_prefix("Image=")
            && let Some(image) = resolve_image(&path_from_uri(value))
        {
            return Some(image);
        }
    }
    None
}

fn resolve_image(path: &Path) -> Option<PathBuf> {
    if path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("xml"))
    {
        return None;
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_file_uris() {
        assert_eq!(
            path_from_uri("file:///home/a/My%20Pics/x.png"),
            Path::new("/home/a/My Pics/x.png")
        );
        assert_eq!(path_from_uri("/caf%C3%A9.jpg"), Path::new("/café.jpg"));
        assert_eq!(path_from_uri("/100%"), Path::new("/100%"));
    }

    #[test]
    fn parses_wallpaper_tool_output() {
        let cosmic = r#"(output: "all", source: Path("/usr/share/backgrounds/cosmic/orion.jpg"), filter_by_theme: true)"#;
        assert_eq!(
            parse_cosmic(cosmic),
            Some("/usr/share/backgrounds/cosmic/orion.jpg")
        );
        assert_eq!(
            parse_hyprpaper("eDP-1 = /home/a/wall.png\n"),
            Some("/home/a/wall.png")
        );
        assert_eq!(
            parse_swww(": eDP-1: 1920x1080, scale: 1, currently displaying: image: /home/a/w.jpg"),
            Some("/home/a/w.jpg")
        );
        assert_eq!(
            parse_swaybg("4242 swaybg -o * -i /home/a/w.jpg -m fill"),
            Some("/home/a/w.jpg")
        );
        assert_eq!(parse_swaybg("4242 swaybg -c #000000"), None);
    }

    #[test]
    fn skips_slideshow_definitions() {
        assert_eq!(
            resolve_image(Path::new("/usr/share/backgrounds/gnome/adwaita-timed.xml")),
            None
        );
    }
}
