//! A real create -> update -> checkpoint -> backup -> migrate -> reopen flow.
//! Usage: cargo run -p skrin --example lifecycle -- NEW_DIRECTORY NEW_BACKUP_DIRECTORY
use skrin::{Database, Decoder, Encoder, Error, Record, Result, Schema};
use std::path::PathBuf;

struct Person {
    full_name: String,
}
impl Record for Person {
    const SCHEMA: Schema = Schema {
        table_id: 42,
        version: 1,
    };
    fn encode(&self, encoder: &mut Encoder) -> Result<()> {
        encoder.string(&self.full_name)
    }
    fn decode(decoder: &mut Decoder<'_>) -> Result<Self> {
        Ok(Self {
            full_name: decoder.string()?.into(),
        })
    }
}

#[derive(Debug)]
struct PersonV2 {
    first_name: String,
    last_name: String,
    active: bool,
}
impl Record for PersonV2 {
    const SCHEMA: Schema = Schema {
        table_id: 42,
        version: 2,
    };
    fn encode(&self, encoder: &mut Encoder) -> Result<()> {
        encoder.string(&self.first_name)?;
        encoder.string(&self.last_name)?;
        encoder.u8(u8::from(self.active))
    }
    fn decode(decoder: &mut Decoder<'_>) -> Result<Self> {
        let first_name = decoder.string()?.into();
        let last_name = decoder.string()?.into();
        let active = match decoder.u8()? {
            0 => false,
            1 => true,
            _ => return Err(Error::Codec("invalid boolean".into())),
        };
        Ok(Self {
            first_name,
            last_name,
            active,
        })
    }
}

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    if args.len() != 2 {
        return Err(Error::InvalidOperation(
            "usage: lifecycle NEW_DIRECTORY NEW_BACKUP_DIRECTORY".into(),
        ));
    }
    let db = Database::<Person>::create_dir(&args[0])?;
    db.write(|tx| {
        tx.insert(
            1,
            Person {
                full_name: "Micke Skrin".into(),
            },
        )
    })?;
    db.write(|tx| {
        tx.update(1, |person| {
            Ok(Person {
                full_name: person.full_name.to_uppercase(),
            })
        })
    })?;
    println!("Checkpoint: {:?}", db.checkpoint()?);
    let backup = db.backup_to(&args[1])?;
    assert_eq!(backup.read()?.get(1).unwrap().full_name, "MICKE SKRIN");
    println!("Verified backup: {:?}", backup.stats()?);
    drop(backup);

    let migrated = db.migrate::<PersonV2>("split-name-v2", |_, old| {
        let (first, last) = old
            .full_name
            .split_once(' ')
            .ok_or_else(|| Error::Codec("missing surname".into()))?;
        Ok(PersonV2 {
            first_name: first.into(),
            last_name: last.into(),
            active: true,
        })
    })?;
    println!("Migration history: {:?}", migrated.generation_info()?);
    println!("Cleanup: {:?}", migrated.prune()?);
    drop(migrated);

    assert!(matches!(
        Database::<Person>::open_dir(&args[0]),
        Err(Error::SchemaMismatch { .. })
    ));
    let reopened = Database::<PersonV2>::open_dir(&args[0])?;
    assert_eq!(reopened.stats()?.commits, 2);
    let read = reopened.read()?;
    let person = read.get(1).unwrap();
    assert_eq!(person.first_name, "MICKE");
    assert_eq!(person.last_name, "SKRIN");
    assert!(person.active);
    println!("Reopened the migrated database: {person:?}");
    // The independent backup is deliberately still schema v1.
    assert_eq!(Database::<Person>::open_dir(&args[1])?.stats()?.commits, 2);
    Ok(())
}
