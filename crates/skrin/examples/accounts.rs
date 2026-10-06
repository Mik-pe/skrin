use skrin::{Database, Decoder, Encoder, Record, Result, Schema};
use std::path::PathBuf;

#[derive(Debug)]
struct Account {
    name: String,
    balance: u64,
}

impl Record for Account {
    const SCHEMA: Schema = Schema {
        table_id: 1,
        version: 1,
    };

    fn encode(&self, encoder: &mut Encoder) -> Result<()> {
        encoder.string(&self.name)?;
        encoder.u64(self.balance)
    }

    fn decode(decoder: &mut Decoder<'_>) -> Result<Self> {
        Ok(Self {
            name: decoder.string()?.to_owned(),
            balance: decoder.u64()?,
        })
    }
}

fn main() -> Result<()> {
    let path = std::env::args_os().nth(1).map(PathBuf::from);
    let db = match &path {
        Some(path) => Database::<Account>::create(path)?,
        None => Database::<Account>::in_memory(),
    };

    db.write(|tx| {
        tx.insert(
            42,
            Account {
                name: "Alice".into(),
                balance: 100,
            },
        )?;
        tx.insert(
            7,
            Account {
                name: "Bob".into(),
                balance: 100,
            },
        )
    })?;

    {
        let read = db.read()?;
        for (key, account) in read.iter() {
            println!("{key}: {} has {}", account.name, account.balance);
        }
    }
    println!("Committed state: {:?}", db.stats()?);
    drop(db); // Release the lock before reopening, not just the read guard.

    if let Some(path) = path {
        let reopened = Database::<Account>::open(path)?;
        let read = reopened.read()?;
        assert_eq!(read.len(), 2);
        assert_eq!(read.get(42).unwrap().balance, 100);
        assert_eq!(read.get(7).unwrap().name, "Bob");
        println!("Verified {} records after reopening", read.len());
    }
    Ok(())
}
