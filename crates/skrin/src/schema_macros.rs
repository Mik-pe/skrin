/// Declare a typed catalog using existing record codecs and index projections.
///
/// Generates the catalog marker, native row enum, typed table markers and index
/// ID constants with the specified visibility. Optional attributes before the
/// row enum name apply only to that enum. Record types can use `Record` derive
/// or manual codecs. Table and index declarations must be strictly ID-sorted;
/// the engine validates the complete descriptor before storage is created.
/// Every index projection is ordinary Rust returning `Result<Vec<u8>>` and
/// receives the declared table's typed record. Projections must be pure and
/// deterministic. Index IDs, versions, uniqueness and key bytes remain explicit
/// persistent contracts. Changing them requires a catalog migration.
///
/// ```
/// #[derive(Debug, skrin::Record)]
/// #[skrin(table_id = 1, version = 1)]
/// struct Player { area: u64, name: String }
/// skrin::catalog! {
///     Game, #[derive(Debug)] Row {
///         schema: (100, 1),
///         tables: { Players: Player },
///         indexes: {
///             BY_AREA: Players {
///                 id: 1, version: 1, unique: false,
///                 key: |player: &Player| Ok(player.area.to_be_bytes().to_vec())
///             }
///         }
///     }
/// }
/// let db = skrin::catalog::CatalogDatabase::<Game>::in_memory()?;
/// db.write(|tx| tx.insert::<Players>(42, Player { area: 3, name: "Ada".into() }))?;
/// let read = db.read()?;
/// assert_eq!(read.lookup::<Players>(BY_AREA, &3_u64.to_be_bytes())?[0].1.name, "Ada");
/// # Ok::<(), skrin::Error>(())
/// ```
///
/// An index projection must accept its declared record type:
/// ```compile_fail
/// #[derive(skrin::Record)]
/// #[skrin(table_id = 1, version = 1)]
/// struct Player { area: u64 }
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
        $($vis const $index: u64 = $index_id;)*
        impl $crate::catalog::Catalog for $catalog {
            const SCHEMA: $crate::Schema = $crate::Schema {
                table_id: $catalog_id, version: $catalog_version
            };
            const TABLES: &'static [$crate::Schema] = &[
                $(<$record as $crate::Record>::SCHEMA),+
            ];
            const INDEXES: &'static [$crate::catalog::IndexDefinition] = &[
                $($crate::catalog::IndexDefinition {
                    id: $index,
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
                    $((id, $row::$index_table(record)) if id == $index => ($projection)(record),)*
                    _ => ::core::result::Result::Err($crate::Error::Codec("catalog index/table mismatch".into())),
                }
            }
        }
    };
}
