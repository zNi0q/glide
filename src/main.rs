mod background;
mod camera;
mod gpu;
mod record;
mod render;
mod ui;

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

use crate::background::Background;

pub const FPS: u32 = 60;
pub const SCREEN_FILE: &str = "screen.mkv";
pub const CURSOR_FILE: &str = "cursor.txt";

#[derive(Parser)]
#[command(
    version,
    about = "Record a window, then render it as a polished video with a cursor-following camera"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    #[command(about = "Show the floating recording toolbar")]
    Ui,
    #[command(about = "Record a window (chosen in the system dialog). Stop with Ctrl+C")]
    Record {
        #[arg(help = "Directory to store the raw recording in")]
        dir: PathBuf,
    },
    #[command(about = "Take a polished screenshot of a window (chosen in the system dialog)")]
    Screenshot {
        #[arg(short, long, default_value = "glide.png", help = "Output image file")]
        output: PathBuf,
        #[arg(long, help = "Capture the whole screen instead of a window")]
        screen: bool,
        #[arg(
            long,
            help = "Background image (defaults to your desktop wallpaper, else a gradient)"
        )]
        background: Option<PathBuf>,
    },
    #[command(about = "Render a recording into a finished video")]
    Render {
        #[arg(help = "Directory produced by `glide record`")]
        dir: PathBuf,
        #[arg(short, long, default_value = "glide.mp4", help = "Output video file")]
        output: PathBuf,
        #[arg(
            long,
            num_args = 0..=1,
            default_missing_value = "1.8",
            value_parser = parse_zoom,
            help = "Zoom in and follow the cursor, up to this factor (default 1.8 when given without a value)"
        )]
        zoom: Option<f32>,
        #[arg(
            long,
            help = "Background image (defaults to your desktop wallpaper, else a gradient)"
        )]
        background: Option<PathBuf>,
    },
}

fn parse_zoom(s: &str) -> Result<f32, String> {
    match s.parse::<f32>() {
        Ok(z) if (1.0..=4.0).contains(&z) => Ok(z),
        _ => Err("zoom must be a number between 1 and 4".into()),
    }
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Ui => ui::run(),
        Command::Record { dir } => record::run_cli(&dir),
        Command::Screenshot {
            output,
            screen,
            background,
        } => {
            let source = if screen {
                record::Source::Screen
            } else {
                record::Source::Window
            };
            let image = record::screenshot(source)?;
            let background = background.map_or(Background::Wallpaper, Background::Image);
            render::still(&image, &output, &background)?;
            println!("Saved {}", output.display());
            Ok(())
        }
        Command::Render {
            dir,
            output,
            zoom,
            background,
        } => {
            let background = background.map_or(Background::Wallpaper, Background::Image);
            render::run(
                &dir,
                &output,
                zoom.unwrap_or(1.0),
                &background,
                |done, total| {
                    if done % FPS as usize == 0 || done == total {
                        eprint!("\rRendered {done}/{total} frames");
                    }
                },
            )?;
            eprintln!();
            println!("Wrote {}", output.display());
            Ok(())
        }
    }
}
