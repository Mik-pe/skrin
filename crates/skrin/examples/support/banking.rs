use skrin::catalog::{Catalog, CatalogDatabase, CatalogWrite, IndexDefinition, Table};
use skrin::{Decoder, Encoder, Error, Record, Result, Schema};

#[derive(Debug, PartialEq, Eq)]
pub struct Account {
    pub email: String,
    pub balance: u64,
}
impl Record for Account {
    const SCHEMA: Schema = Schema {
        table_id: 1,
        version: 1,
    };
    fn encode(&self, e: &mut Encoder) -> Result<()> {
        e.string(&self.email)?;
        e.u64(self.balance)
    }
    fn decode(d: &mut Decoder<'_>) -> Result<Self> {
        Ok(Self {
            email: d.string()?.into(),
            balance: d.u64()?,
        })
    }
}
#[derive(Debug, PartialEq, Eq)]
pub struct Transfer {
    pub from: u64,
    pub to: u64,
    pub amount: u64,
}
impl Record for Transfer {
    const SCHEMA: Schema = Schema {
        table_id: 2,
        version: 1,
    };
    fn encode(&self, e: &mut Encoder) -> Result<()> {
        e.u64(self.from)?;
        e.u64(self.to)?;
        e.u64(self.amount)
    }
    fn decode(d: &mut Decoder<'_>) -> Result<Self> {
        Ok(Self {
            from: d.u64()?,
            to: d.u64()?,
            amount: d.u64()?,
        })
    }
}
pub enum Row {
    Account(Account),
    Transfer(Transfer),
}
pub struct Banking;
impl Catalog for Banking {
    const SCHEMA: Schema = Schema {
        table_id: 8000,
        version: 1,
    };
    const TABLES: &'static [Schema] = &[Account::SCHEMA, Transfer::SCHEMA];
    const INDEXES: &'static [IndexDefinition] = &[
        IndexDefinition {
            id: 1,
            table_id: 1,
            version: 1,
            unique: true,
        },
        IndexDefinition {
            id: 2,
            table_id: 1,
            version: 1,
            unique: false,
        },
    ];
    type Row = Row;
    fn table_id(row: &Row) -> u64 {
        match row {
            Row::Account(_) => 1,
            Row::Transfer(_) => 2,
        }
    }
    fn encode(row: &Row, e: &mut Encoder) -> Result<()> {
        match row {
            Row::Account(r) => r.encode(e),
            Row::Transfer(r) => r.encode(e),
        }
    }
    fn decode(id: u64, d: &mut Decoder<'_>) -> Result<Row> {
        match id {
            1 => Ok(Row::Account(Account::decode(d)?)),
            2 => Ok(Row::Transfer(Transfer::decode(d)?)),
            _ => Err(Error::Codec("unknown table".into())),
        }
    }
    fn index_key(id: u64, row: &Row) -> Result<Vec<u8>> {
        match (id, row) {
            (1, Row::Account(r)) => Ok(r.email.as_bytes().to_vec()),
            // Big endian makes lexicographic ordering match unsigned numbers.
            (2, Row::Account(r)) => Ok(r.balance.to_be_bytes().to_vec()),
            _ => Err(Error::Codec("index/table mismatch".into())),
        }
    }
}
pub struct Accounts;
impl Table<Banking> for Accounts {
    type Record = Account;
    fn into_row(r: Account) -> Row {
        Row::Account(r)
    }
    fn borrow(r: &Row) -> Option<&Account> {
        match r {
            Row::Account(r) => Some(r),
            _ => None,
        }
    }
}
pub struct Transfers;
impl Table<Banking> for Transfers {
    type Record = Transfer;
    fn into_row(r: Transfer) -> Row {
        Row::Transfer(r)
    }
    fn borrow(r: &Row) -> Option<&Transfer> {
        match r {
            Row::Transfer(r) => Some(r),
            _ => None,
        }
    }
}
pub fn seed(db: &CatalogDatabase<Banking>) -> Result<()> {
    db.write(|tx| {
        tx.insert::<Accounts>(
            1,
            Account {
                email: "alice@example.test".into(),
                balance: 100,
            },
        )?;
        tx.insert::<Accounts>(
            2,
            Account {
                email: "bob@example.test".into(),
                balance: 100,
            },
        )
    })
}
pub fn transfer(
    tx: &mut CatalogWrite<'_, Banking>,
    operation_id: u64,
    from: u64,
    to: u64,
    amount: u64,
) -> Result<()> {
    if from == to {
        return Err(Error::InvalidOperation(
            "transfer requires distinct accounts".into(),
        ));
    }
    tx.update::<Accounts>(from, |r| {
        Ok(Account {
            email: r.email.clone(),
            balance: r
                .balance
                .checked_sub(amount)
                .ok_or_else(|| Error::InvalidOperation("insufficient balance".into()))?,
        })
    })?;
    tx.update::<Accounts>(to, |r| {
        Ok(Account {
            email: r.email.clone(),
            balance: r
                .balance
                .checked_add(amount)
                .ok_or_else(|| Error::InvalidOperation("balance overflow".into()))?,
        })
    })?;
    // A duplicate operation ID propagates out of the closure, rolling back both balances.
    tx.insert::<Transfers>(operation_id, Transfer { from, to, amount })
}
