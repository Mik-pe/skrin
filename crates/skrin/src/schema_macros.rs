/// Declare a typed catalog using existing record codecs and index projections.
///
/// Generates the catalog marker, native row enum, typed table markers and typed
/// index markers (or raw ID constants) with the specified visibility. Optional
/// attributes before the row enum name apply only to that enum. Record types can use `Record` derive
/// or manual codecs. Table and index declarations must be strictly ID-sorted;
/// the engine validates the complete descriptor before storage is created.
/// Typed `key: Key => |row| expression` declarations generate an index marker
/// implementing `catalog::Index`, sharing `Key::encode_key` between projections
/// and queries. The row parameter is inferred and the expression may borrow
/// strings/bytes or use `?` for a fallible projection. Declare all indexes in
/// this typed form, or use the original `key: |row: &Record| -> Result<Vec<u8>>`
/// form to generate raw ID constants. Projections must be pure and deterministic.
/// Index IDs, versions, uniqueness and key bytes remain persistent contracts;
/// changing them requires a catalog migration.
///
/// ```
/// #[derive(Debug)]
/// struct Player { area: u64, name: String }
/// impl skrin::Record for Player {
///     const SCHEMA: skrin::Schema = skrin::Schema { table_id: 1, version: 1 };
///     fn encode(&self, e: &mut skrin::Encoder) -> skrin::Result<()> {
///         e.u64(self.area)?;
///         e.string(&self.name)
///     }
///     fn decode(d: &mut skrin::Decoder<'_>) -> skrin::Result<Self> {
///         Ok(Self { area: d.u64()?, name: d.string()?.into() })
///     }
/// }
/// skrin::catalog! {
///     Game, #[derive(Debug)] Row {
///         schema: (100, 1),
///         tables: { Players: Player },
///         indexes: {
///             ByArea: Players {
///                 id: 1, version: 1, unique: false,
///                 key: u64 => |player| player.area
///             }
///         }
///     }
/// }
/// let db = skrin::catalog::CatalogDatabase::<Game>::in_memory()?;
/// db.write(|tx| tx.insert::<Players>(42, Player { area: 3, name: "Ada".into() }))?;
/// let read = db.read()?;
/// assert_eq!(read.matching(ByArea, &3)?.next().unwrap().1.name, "Ada");
/// assert_eq!(read.query(ByArea, 0..=3)?.take(1).count(), 1);
/// # Ok::<(), skrin::Error>(())
/// ```
///
/// An index projection must accept its declared record type:
/// ```compile_fail
/// struct Player { area: u64 }
/// impl skrin::Record for Player {
///     const SCHEMA: skrin::Schema = skrin::Schema { table_id: 1, version: 1 };
///     fn encode(&self, e: &mut skrin::Encoder) -> skrin::Result<()> { e.u64(self.area) }
///     fn decode(d: &mut skrin::Decoder<'_>) -> skrin::Result<Self> { Ok(Self { area: d.u64()? }) }
/// }
/// skrin::catalog! {
///     Game, Row {
///         schema: (100, 1),
///         tables: { Players: Player },
///         indexes: {
///             BY_AREA: Players {
///                 id: 1, version: 1, unique: false,
///                 key: |wrong_type: &u64| Ok(wrong_type.to_be_bytes().to_vec())
///             }
///         }
///     }
/// }
/// ```
#[macro_export]
macro_rules! catalog {
    (
        $vis:vis $catalog:ident, $(#[$row_attr:meta])* $row:ident {
            schema: ($catalog_id:expr, $catalog_version:expr),
            tables: { $($table:ident: $record:ty),+ $(,)? },
            indexes: {
                $($index:ident: $index_table:ident {
                    id: $index_id:expr, version: $index_version:expr,
                    unique: $unique:expr, key: $key:ty => |$value:ident| $projection:expr $(,)?
                }),+ $(,)?
            } $(,)?
        }
    ) => {
        $(
            $vis struct $index;
            impl $crate::catalog::Index<$catalog> for $index {
                type Table = $index_table;
                type Key = $key;
                const DEFINITION: $crate::catalog::IndexDefinition = $crate::catalog::IndexDefinition {
                    id: $index_id,
                    table_id: <<$index_table as $crate::catalog::Table<$catalog>>::Record as $crate::Record>::SCHEMA.table_id,
                    version: $index_version,
                    unique: $unique,
                };
                fn project($value: &<$index_table as $crate::catalog::Table<$catalog>>::Record) -> $crate::Result<::std::vec::Vec<u8>> {
                    <$key as $crate::catalog::IndexKey>::encode_key(&($projection))
                }
            }
        )+
        $crate::catalog! { @catalog
            $vis $catalog, $(#[$row_attr])* $row {
                schema: ($catalog_id, $catalog_version),
                tables: { $($table: $record),+ },
                indexes: { $($index: $index_table {
                    id: $index_id, version: $index_version, unique: $unique,
                    key: <$index as $crate::catalog::Index<$catalog>>::project
                }),+ }
            }
        }
    };
    (
        $vis:vis $catalog:ident, $(#[$row_attr:meta])* $row:ident {
            schema: ($catalog_id:expr, $catalog_version:expr),
            tables: { $($table:ident: $record:ty),+ $(,)? },
            indexes: {
                $($index:ident: $index_table:ident {
                    id: $index_id:expr, version: $index_version:expr,
                    unique: $unique:expr, key: $projection:expr $(,)?
                }),* $(,)?
            } $(,)?
        }
    ) => {
        $($vis const $index: u64 = $index_id;)*
        $crate::catalog! { @catalog
            $vis $catalog, $(#[$row_attr])* $row {
                schema: ($catalog_id, $catalog_version),
                tables: { $($table: $record),+ },
                indexes: { $($index: $index_table {
                    id: $index_id, version: $index_version,
                    unique: $unique, key: $projection
                }),* }
            }
        }
    };
    (@catalog
        $vis:vis $catalog:ident, $(#[$row_attr:meta])* $row:ident {
            schema: ($catalog_id:expr, $catalog_version:expr),
            tables: { $($table:ident: $record:ty),+ $(,)? },
            indexes: {
                $($index:ident: $index_table:ident {
                    id: $index_id:expr, version: $index_version:expr,
                    unique: $unique:expr, key: $projection:expr $(,)?
                }),* $(,)?
            } $(,)?
        }
    ) => {
        $vis struct $catalog;
        $(#[$row_attr])*
        $vis enum $row {
            $($table($record)),+
        }
        $(
            $vis struct $table;
            impl $crate::catalog::Table<$catalog> for $table {
                type Record = $record;
                fn into_row(record: $record) -> $row { $row::$table(record) }
                // A one-table catalog has an irrefutable variant; the same
                // generated borrow also supports catalogs with other tables.
                #[allow(irrefutable_let_patterns)]
                fn borrow(row: &$row) -> ::core::option::Option<&$record> {
                    if let $row::$table(record) = row {
                        ::core::option::Option::Some(record)
                    } else {
                        ::core::option::Option::None
                    }
                }
            }
        )+
        impl $crate::catalog::Catalog for $catalog {
            const SCHEMA: $crate::Schema = $crate::Schema {
                table_id: $catalog_id, version: $catalog_version
            };
            const TABLES: &'static [$crate::Schema] = &[
                $(<$record as $crate::Record>::SCHEMA),+
            ];
            const INDEXES: &'static [$crate::catalog::IndexDefinition] = &[
                $($crate::catalog::IndexDefinition {
                    id: $index_id,
                    table_id: <<$index_table as $crate::catalog::Table<$catalog>>::Record as $crate::Record>::SCHEMA.table_id,
                    version: $index_version,
                    unique: $unique,
                }),*
            ];
            type Row = $row;
            fn table_id(row: &$row) -> u64 {
                match row { $($row::$table(_) => <$record as $crate::Record>::SCHEMA.table_id),+ }
            }
            fn encode(row: &$row, encoder: &mut $crate::Encoder) -> $crate::Result<()> {
                match row { $($row::$table(record) => <$record as $crate::Record>::encode(record, encoder)),+ }
            }
            fn decode(table_id: u64, decoder: &mut $crate::Decoder<'_>) -> $crate::Result<$row> {
                $(if table_id == <$record as $crate::Record>::SCHEMA.table_id {
                    return <$record as $crate::Record>::decode(decoder).map($row::$table);
                })+
                ::core::result::Result::Err($crate::Error::Codec("unknown catalog table".into()))
            }
            fn index_key(index_id: u64, row: &$row) -> $crate::Result<::std::vec::Vec<u8>> {
                // The fallback also handles no-index catalogs without allowing
                // accidental projection of a different row variant.
                let _ = (index_id, row);
                match (index_id, row) {
                    $((id, $row::$index_table(record)) if id == $index_id => ($projection)(record),)*
                    _ => ::core::result::Result::Err($crate::Error::Codec("catalog index/table mismatch".into())),
                }
            }
        }
    };
}
