//! Generate Skrin's explicit-schema, field-ordered record codecs.
#![forbid(unsafe_code)]

use proc_macro::TokenStream;
use proc_macro2::TokenStream as Tokens;
use quote::{ToTokens, quote};
use syn::{Data, DeriveInput, Fields, GenericArgument, LitInt, Path, PathArguments, Type};

/// Derive a fixed field-order codec, requiring explicit table and schema IDs.
#[proc_macro_derive(Record, attributes(skrin))]
pub fn record(input: TokenStream) -> TokenStream {
    syn::parse(input)
        .and_then(expand)
        .unwrap_or_else(|error| error.into_compile_error())
        .into()
}

fn expand(input: DeriveInput) -> syn::Result<Tokens> {
    if !input.generics.params.is_empty() || input.generics.where_clause.is_some() {
        return Err(syn::Error::new_spanned(
            input.generics,
            "Record derive requires a concrete type; implement Record manually for generic models",
        ));
    }
    let mut table_id = None;
    let mut version = None;
    let mut crate_path: Option<Path> = None;
    for attr in input.attrs.iter().filter(|a| a.path().is_ident("skrin")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("table_id") {
                if table_id.is_some() {
                    return Err(meta.error("duplicate table_id"));
                }
                let value: LitInt = meta.value()?.parse()?;
                value.base10_parse::<u64>()?;
                table_id = Some(value);
            } else if meta.path.is_ident("version") {
                if version.is_some() {
                    return Err(meta.error("duplicate version"));
                }
                let value: LitInt = meta.value()?.parse()?;
                value.base10_parse::<u32>()?;
                version = Some(value);
            } else if meta.path.is_ident("crate") {
                if crate_path.is_some() {
                    return Err(meta.error("duplicate crate path"));
                }
                crate_path = Some(meta.value()?.parse()?);
            } else {
                return Err(meta.error("expected table_id, version or crate"));
            }
            Ok(())
        })?;
    }
    let table_id = table_id.ok_or_else(|| {
        syn::Error::new_spanned(
            &input.ident,
            "Record derive requires #[skrin(table_id = ..., version = ...)]",
        )
    })?;
    let version = version.ok_or_else(|| {
        syn::Error::new_spanned(
            &input.ident,
            "Record derive requires an explicit schema version",
        )
    })?;
    let path = crate_path.map_or_else(|| quote!(::skrin), |p| p.to_token_stream());
    let Data::Struct(data) = input.data else {
        return Err(syn::Error::new_spanned(
            input.ident,
            "Record derive supports named structs; implement Record manually for enums/unions",
        ));
    };
    let Fields::Named(fields) = data.fields else {
        return Err(syn::Error::new_spanned(
            input.ident,
            "Record derive requires named fields",
        ));
    };
    let mut encodings = Vec::new();
    let mut decodings = Vec::new();
    for field in fields.named {
        if let Some(attr) = field.attrs.iter().find(|a| a.path().is_ident("skrin")) {
            return Err(syn::Error::new_spanned(
                attr,
                "Record derive persists every field in declaration order; field attributes are unsupported",
            ));
        }
        let name = field.ident.expect("named field");
        let (encode, decode) = match kind(&field.ty) {
            Some("u8") => (
                quote!(__skrin_encoder.u8(self.#name)?;),
                quote!(__skrin_decoder.u8()?),
            ),
            Some("u32") => (
                quote!(__skrin_encoder.u32(self.#name)?;),
                quote!(__skrin_decoder.u32()?),
            ),
            Some("u64") => (
                quote!(__skrin_encoder.u64(self.#name)?;),
                quote!(__skrin_decoder.u64()?),
            ),
            Some("String") => (
                quote!(__skrin_encoder.string(&self.#name)?;),
                quote!(__skrin_decoder.string()?.to_owned()),
            ),
            Some("bytes") => (
                quote!(__skrin_encoder.bytes(&self.#name)?;),
                quote!(__skrin_decoder.bytes()?.to_vec()),
            ),
            _ => {
                return Err(syn::Error::new_spanned(
                    field.ty,
                    "Record derive supports u8, u32, u64, String and Vec<u8>; implement Record manually for other codecs",
                ));
            }
        };
        encodings.push(encode);
        decodings.push(quote!(#name: #decode));
    }
    let name = input.ident;
    Ok(quote! {
        impl #path::Record for #name {
            const SCHEMA: #path::Schema = #path::Schema { table_id: #table_id, version: #version };
            fn encode(&self, __skrin_encoder: &mut #path::Encoder) -> #path::Result<()> {
                #(#encodings)*
                ::core::result::Result::Ok(())
            }
            fn decode(__skrin_decoder: &mut #path::Decoder<'_>) -> #path::Result<Self> {
                ::core::result::Result::Ok(Self { #(#decodings),* })
            }
        }
    })
}

fn kind(ty: &Type) -> Option<&'static str> {
    let Type::Path(p) = ty else { return None };
    if p.qself.is_some() {
        return None;
    }
    let segments = &p.path.segments;
    let last = segments.last()?;
    let prefix: Vec<_> = segments
        .iter()
        .take(segments.len() - 1)
        .map(|s| s.ident.to_string())
        .collect();
    let standard =
        prefix.is_empty() || prefix == ["std", "string"] || prefix == ["alloc", "string"];
    if matches!(last.arguments, PathArguments::None) {
        return match last.ident.to_string().as_str() {
            "u8" if prefix.is_empty() => Some("u8"),
            "u32" if prefix.is_empty() => Some("u32"),
            "u64" if prefix.is_empty() => Some("u64"),
            "String" if standard => Some("String"),
            _ => None,
        };
    }
    if last.ident == "Vec"
        && (prefix.is_empty() || prefix == ["std", "vec"] || prefix == ["alloc", "vec"])
        && let PathArguments::AngleBracketed(args) = &last.arguments
        && args.args.len() == 1
        && let Some(GenericArgument::Type(inner)) = args.args.first()
        && kind(inner) == Some("u8")
    {
        return Some("bytes");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_ambiguous_or_incomplete_schemas_and_unsupported_state() {
        let cases = [
            ("struct A { n: u64 }", "requires #[skrin"),
            (
                "#[skrin(table_id=1)] struct A { n: u64 }",
                "explicit schema version",
            ),
            (
                "#[skrin(table_id=1, table_id=2, version=1)] struct A { n: u64 }",
                "duplicate table_id",
            ),
            (
                "#[skrin(table_id=1, version=1, version=2)] struct A { n: u64 }",
                "duplicate version",
            ),
            (
                "#[skrin(table_id=1, version=1, default)] struct A { n: u64 }",
                "expected table_id",
            ),
            (
                "#[skrin(table_id=1, version=4294967296)] struct A { n: u64 }",
                "too large",
            ),
            (
                "#[skrin(table_id=1, version=1)] struct A { n: usize }",
                "supports u8",
            ),
            (
                "#[skrin(table_id=1, version=1)] struct A { n: Vec<u64> }",
                "supports u8",
            ),
            (
                "#[skrin(table_id=1, version=1)] struct A<T> { n: T }",
                "concrete type",
            ),
            (
                "#[skrin(table_id=1, version=1)] struct A(u64);",
                "named fields",
            ),
            (
                "#[skrin(table_id=1, version=1)] enum A { B }",
                "named structs",
            ),
            (
                "#[skrin(table_id=1, version=1)] struct A { #[skrin(skip)] n: u64 }",
                "persists every field",
            ),
        ];
        for (source, expected) in cases {
            let err = expand(syn::parse_str(source).unwrap()).unwrap_err();
            assert!(err.to_string().contains(expected), "{source}: {err}");
        }
    }

    #[test]
    fn recognizes_only_documented_field_spellings() {
        for source in [
            "u8",
            "u32",
            "u64",
            "String",
            "std::string::String",
            "::std::vec::Vec<u8>",
        ] {
            assert!(kind(&syn::parse_str(source).unwrap()).is_some(), "{source}");
        }
        for source in [
            "Option<u64>",
            "usize",
            "&'static str",
            "other::String",
            "Vec<String>",
            "[u8; 8]",
        ] {
            assert!(kind(&syn::parse_str(source).unwrap()).is_none(), "{source}");
        }
    }
}
