//! Model -> native query -> atomic edit -> WAL/checkpoint/backup reopen.
#[path = "support/characters.rs"]
mod characters;
use characters::*;
use skrin::catalog::CatalogDatabase;
use skrin::{Error, Result};
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os()
        .skip(1)
        .map(std::path::PathBuf::from)
        .collect();
    if args.len() > 1 {
        return Err(Error::InvalidOperation(
            "usage: game_models [NEW_DIRECTORY]".into(),
        ));
    }
    let db = match args.first() {
        Some(path) => CatalogDatabase::<Game>::create_dir(path)?,
        None => CatalogDatabase::<Game>::in_memory()?,
    };
    seed(&db)?;
    verify(&db.read()?, false)?;
    db.write(|tx| damage(tx, 7, 125.0))?;
    verify(&db.read()?, true)?;
    if let Some(path) = args.first() {
        drop(db);
        let db = CatalogDatabase::<Game>::open_dir(path)?;
        verify(&db.read()?, true)?;
        db.checkpoint()?;
        db.reclaim()?;
        let backup_path = path.with_extension("models-backup");
        let backup = db.backup_to(&backup_path)?;
        verify(&backup.read()?, true)?;
        drop(backup);
        drop(db);
        for restored in [path, &backup_path] {
            verify(&CatalogDatabase::<Game>::open_dir(restored)?.read()?, true)?;
        }
        println!(
            "signed positions, float health, bool index and optional team verified after WAL/checkpoint/backup reopen"
        );
    } else {
        println!("volatile model, signed query bounds and atomic character edit verified");
    }
    Ok(())
}
