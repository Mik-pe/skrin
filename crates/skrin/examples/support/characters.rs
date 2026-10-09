//! Ordinary game fields: signed positions, float health, flags and optional state.
use skrin::catalog::{CatalogDatabase, CatalogRead, CatalogWrite};
use skrin::{Error, Result};

#[derive(Debug, PartialEq, skrin::Record)]
#[skrin(table_id = 1, version = 1)]
pub struct Character {
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub health: f32,
    pub alive: bool,
    pub team: Option<u64>,
}
skrin::catalog! {
    pub Game, Row {
        schema: (11000, 1), tables: { Characters: Character },
        indexes: {
            Chunk: Characters { id: 1, version: 1, unique: false, key: i32 => |r| r.x.div_euclid(64) },
            Alive: Characters { id: 2, version: 1, unique: false, key: bool => |r| r.alive }
        }
    }
}
pub fn seed(db: &CatalogDatabase<Game>) -> Result<()> {
    db.write(|tx| {
        for (id, x, team) in [(7, -32, None), (9, 16, Some(42))] {
            tx.insert::<Characters>(
                id,
                Character {
                    name: format!("player-{id}"),
                    x,
                    y: -8,
                    health: 100.0,
                    alive: true,
                    team,
                },
            )?;
        }
        Ok(())
    })
}
/// Update a native record without requiring Clone; the complete replacement is
/// staged alongside both derived indexes and published only after commit.
pub fn damage(tx: &mut CatalogWrite<'_, Game>, id: u64, amount: f32) -> Result<()> {
    if !amount.is_finite() || amount < 0.0 {
        return Err(Error::InvalidOperation(
            "damage must be finite and nonnegative".into(),
        ));
    }
    tx.update::<Characters>(id, |old| {
        if !old.health.is_finite() || old.health < 0.0 {
            return Err(Error::InvalidOperation(
                "stored health must be finite and nonnegative".into(),
            ));
        }
        let health = (old.health - amount).max(0.0);
        Ok(Character {
            name: old.name.clone(),
            x: old.x,
            y: old.y,
            health,
            alive: health > 0.0,
            team: old.team,
        })
    })
}
pub fn visible(read: &CatalogRead<'_, Game>) -> Result<Vec<u64>> {
    Ok(read
        .query(Chunk, -1..=0)?
        .filter(|(_, r)| r.alive && r.health > 0.0 && r.y >= -16)
        .take(8)
        .map(|(id, _)| id)
        .collect())
}
pub fn verify(read: &CatalogRead<'_, Game>, damaged: bool) -> Result<()> {
    assert_eq!(read.sequence(), if damaged { 2 } else { 1 });
    let first = read.get::<Characters>(7)?.unwrap();
    assert_eq!(first.name, "player-7");
    assert_eq!((first.x, first.y, first.team), (-32, -8, None));
    assert_eq!(
        (first.health, first.alive),
        if damaged { (0.0, false) } else { (100.0, true) }
    );
    let second = read.get::<Characters>(9)?.unwrap();
    assert_eq!((second.x, second.y, second.team), (16, -8, Some(42)));
    assert_eq!((second.health, second.alive), (100.0, true));
    assert_eq!(
        read.matching(Alive, &false)?
            .map(|(id, _)| id)
            .collect::<Vec<_>>(),
        if damaged { vec![7] } else { vec![] }
    );
    assert_eq!(visible(read)?, if damaged { vec![9] } else { vec![7, 9] });
    Ok(())
}
