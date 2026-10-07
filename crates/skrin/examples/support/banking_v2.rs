use super::banking::{Account, Banking, Row, Transfer, Transfers};
use skrin::catalog::{Catalog, IndexDefinition, Table};
use skrin::{Decoder, Encoder, Error, Record, Result, Schema};

#[derive(Debug, PartialEq, Eq)]
pub struct AccountV2 {
    pub email: String,
    pub balance: u64,
    pub active: bool,
}
impl Record for AccountV2 {
    const SCHEMA: Schema = Schema {
        table_id: 1,
        version: 2,
    };
    fn encode(&self, e: &mut Encoder) -> Result<()> {
        e.string(&self.email)?;
        e.u64(self.balance)?;
        e.u8(u8::from(self.active))
    }
    fn decode(d: &mut Decoder<'_>) -> Result<Self> {
        let email = d.string()?.into();
        let balance = d.u64()?;
        let active = match d.u8()? {
            0 => false,
            1 => true,
            _ => return Err(Error::Codec("invalid active flag".into())),
        };
        Ok(Self {
            email,
            balance,
            active,
        })
    }
}
pub enum RowV2 {
    Account(AccountV2),
    Transfer(Transfer),
}
pub struct BankingV2;
impl Catalog for BankingV2 {
    const SCHEMA: Schema = Schema {
        table_id: 8000,
        version: 2,
    };
    const TABLES: &'static [Schema] = &[AccountV2::SCHEMA, Transfer::SCHEMA];
    const INDEXES: &'static [IndexDefinition] = Banking::INDEXES;
    type Row = RowV2;
    fn table_id(r: &RowV2) -> u64 {
        match r {
            RowV2::Account(_) => 1,
            RowV2::Transfer(_) => 2,
        }
    }
    fn encode(r: &RowV2, e: &mut Encoder) -> Result<()> {
        match r {
            RowV2::Account(r) => r.encode(e),
            RowV2::Transfer(r) => r.encode(e),
        }
    }
    fn decode(id: u64, d: &mut Decoder<'_>) -> Result<RowV2> {
        match id {
            1 => Ok(RowV2::Account(AccountV2::decode(d)?)),
            2 => Ok(RowV2::Transfer(Transfer::decode(d)?)),
            _ => Err(Error::Codec("unknown table".into())),
        }
    }
    fn index_key(id: u64, r: &RowV2) -> Result<Vec<u8>> {
        match (id, r) {
            (1, RowV2::Account(r)) => Ok(r.email.as_bytes().to_vec()),
            (2, RowV2::Account(r)) => Ok(r.balance.to_be_bytes().to_vec()),
            _ => Err(Error::Codec("index/table mismatch".into())),
        }
    }
}
pub struct AccountsV2;
impl Table<BankingV2> for AccountsV2 {
    type Record = AccountV2;
    fn into_row(r: AccountV2) -> RowV2 {
        RowV2::Account(r)
    }
    fn borrow(r: &RowV2) -> Option<&AccountV2> {
        match r {
            RowV2::Account(r) => Some(r),
            _ => None,
        }
    }
}
impl Table<BankingV2> for Transfers {
    type Record = Transfer;
    fn into_row(r: Transfer) -> RowV2 {
        RowV2::Transfer(r)
    }
    fn borrow(r: &RowV2) -> Option<&Transfer> {
        match r {
            RowV2::Transfer(r) => Some(r),
            _ => None,
        }
    }
}
pub fn migrate_row(_: u64, _: u64, r: Row) -> Result<RowV2> {
    Ok(match r {
        Row::Account(Account { email, balance }) => RowV2::Account(AccountV2 {
            email,
            balance,
            active: true,
        }),
        Row::Transfer(r) => RowV2::Transfer(r),
    })
}
