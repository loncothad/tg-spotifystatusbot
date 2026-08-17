use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use tg_spotifystatusbot_render::{
    encode_png, example_card_en, example_card_ja, example_card_ru, render_card, CardKind,
};

fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        Some("render-example") => {
            if let Err(err) = render_examples(args.next().map(PathBuf::from)) {
                eprintln!("xtask render-example failed: {err}");
                return ExitCode::FAILURE;
            }
            ExitCode::SUCCESS
        }
        other => {
            eprintln!(
                "Unknown xtask command: {}\nUsage: cargo xtask render-example [directory]",
                other.unwrap_or("<none>")
            );
            ExitCode::FAILURE
        }
    }
}

fn render_examples(dir: Option<PathBuf>) -> Result<(), Box<dyn std::error::Error>> {
    let dir = dir.unwrap_or_else(|| workspace_root().join("assets"));
    fs::create_dir_all(&dir)?;
    let cards = [
        ("example-status.png", example_card_ja()),
        ("example-status-ru.png", example_card_ru()),
        ("example-status-en.png", example_card_en()),
    ];
    for (name, kind) in cards {
        write_card(&dir.join(name), &kind)?;
        println!("Wrote {}", dir.join(name).display());
    }
    Ok(())
}

fn write_card(path: &std::path::Path, kind: &CardKind) -> Result<(), Box<dyn std::error::Error>> {
    let image = render_card(kind)?;
    fs::write(path, encode_png(&image)?)?;
    Ok(())
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives in the workspace")
        .to_path_buf()
}
