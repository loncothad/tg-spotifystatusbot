use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use tg_spotifystatusbot_render::{encode_png, example_playing_card, render_card};

fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        Some("render-example") => {
            let path = args
                .next()
                .map(PathBuf::from)
                .unwrap_or_else(|| workspace_root().join("assets/example-status.png"));
            if let Err(err) = render_example(&path) {
                eprintln!("xtask render-example failed: {err}");
                return ExitCode::FAILURE;
            }
            println!("Wrote {}", path.display());
            ExitCode::SUCCESS
        }
        other => {
            eprintln!(
                "Unknown xtask command: {}\nUsage: cargo xtask render-example [path]",
                other.unwrap_or("<none>")
            );
            ExitCode::FAILURE
        }
    }
}

fn render_example(path: &PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let image = render_card(&example_playing_card())?;
    fs::write(path, encode_png(&image)?)?;
    Ok(())
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives in the workspace")
        .to_path_buf()
}
