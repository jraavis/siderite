//! Emits `impl Model` and the typed `Field` constants.

use super::meta::static_ident;
use super::plan::ModelPlan;
use proc_macro2::TokenStream;
use quote::quote;

/// The field constants (`User::name`) as an inherent impl.
pub fn field_consts(plan: &ModelPlan<'_>) -> TokenStream {
    let ident = plan.ident;
    let consts = plan.columns().map(|field| {
        let (vis, name, ty, column) = (field.vis, field.ident, field.ty, &field.column);
        let doc = format!("Typed handle of the `{column}` column.");
        quote! {
            #[doc = #doc]
            #[allow(non_upper_case_globals)]
            #vis const #name: ::siderite::orm::Field<#ident, #ty> =
                ::siderite::orm::Field::new(#column);
        }
    });
    quote! {
        #[automatically_derived]
        impl #ident {
            #(#consts)*
        }
    }
}

/// `impl Model for T`.
pub fn model_impl(plan: &ModelPlan<'_>) -> TokenStream {
    let orm = quote!(::siderite::orm);
    let ident = plan.ident;
    let meta = static_ident();
    let pk = plan.pk_field();
    let (pk_ident, pk_ty, pk_column) = (pk.ident, pk.ty, &pk.column);
    let is_unsaved = if pk.options.orm.auto {
        quote!(self.#pk_ident == <#pk_ty as ::core::default::Default>::default())
    } else {
        quote!(false)
    };
    let values = plan.columns().map(|f| {
        let (field, column) = (f.ident, &f.column);
        quote!((#column, #orm::DbType::to_value(&self.#field)))
    });
    let reads = field_reads(plan, &quote!(#orm::read_column_at));
    let takes = field_reads(plan, &quote!(#orm::take_column));
    quote! {
        #[automatically_derived]
        impl #orm::Model for #ident {
            type Pk = #pk_ty;
            const META: &'static #orm::ModelMeta = &#meta;

            fn pk(&self) -> #pk_ty {
                ::core::clone::Clone::clone(&self.#pk_ident)
            }

            fn set_pk(&mut self, value: #orm::Value) -> ::core::result::Result<(), #orm::QueryError> {
                self.#pk_ident = #orm::types::decode(#pk_column, value)?;
                ::core::result::Result::Ok(())
            }

            fn is_unsaved(&self) -> bool {
                #is_unsaved
            }

            fn to_values(&self) -> ::std::vec::Vec<(&'static str, #orm::Value)> {
                ::std::vec![#(#values),*]
            }

            fn from_row(
                row: &#orm::Row,
                prefix: &str,
            ) -> ::core::result::Result<Self, #orm::QueryError> {
                ::core::result::Result::Ok(Self { #(#reads),* })
            }

            fn from_row_mut(
                row: &mut #orm::Row,
                prefix: &str,
            ) -> ::core::result::Result<Self, #orm::QueryError> {
                ::core::result::Result::Ok(Self { #(#takes),* })
            }
        }
    }
}

/// Struct-literal initializers decoding every field with `reader`.
///
/// Args:
///     plan: Parsed model.
///     read: Path of `fn(row, prefix, column, hint)`.
///
/// Returns:
///     One `field: value` initializer per struct field; fields without a
///     column take their default. The hint is the column's position among
///     the model's columns, which is its place in a default projection.
fn field_reads(plan: &ModelPlan<'_>, read: &TokenStream) -> Vec<TokenStream> {
    let mut position = 0usize;
    plan.fields
        .iter()
        .map(|f| {
            let field = f.ident;
            if !f.is_column() {
                return quote!(#field: ::core::default::Default::default());
            }
            let (column, hint) = (&f.column, position);
            position += 1;
            quote!(#field: #read(row, prefix, #column, #hint)?)
        })
        .collect()
}
