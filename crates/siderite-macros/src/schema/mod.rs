//! `#[derive(Schema)]`: the JSON Schema, plus the matching `Dump`
//! implementation.

mod dump;
mod enums;
mod fields;
mod stream;
mod strukt;

use crate::attrs::model::Container;
use crate::diag::Errors;
use crate::docs;
use crate::probe::Hooks;
use proc_macro2::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, parse_quote};

/// Expand `#[derive(Schema)]`.
pub fn derive(input: &DeriveInput) -> syn::Result<TokenStream> {
    let mut errors = Errors::default();
    let container = Container::parse(&input.attrs, &mut errors);
    let hooks = Hooks::for_type(&input.generics, &container);
    let body = match &input.data {
        Data::Struct(data) => strukt::body(data, &container, hooks, &mut errors),
        Data::Enum(data) => enums::body(data, &container, &mut errors),
        Data::Union(_) => {
            errors.spanned(input, "derive(Schema) does not support unions");
            TokenStream::new()
        }
    };
    let keywords = fields::doc_keywords(docs::description(&input.attrs));
    let body = fields::annotate(body, &keywords);
    errors.finish(())?;

    let ident = &input.ident;
    let name = if container.options.inline
        || (container.options.name.is_none() && !input.generics.params.is_empty())
    {
        quote!(::core::option::Option::None)
    } else {
        let name = container
            .options
            .name
            .as_ref()
            .map_or_else(|| ident.to_string(), syn::LitStr::value);
        quote!(::core::option::Option::Some(#name))
    };

    let mut generics = input.generics.clone();
    for param in input.generics.type_params() {
        let ty = &param.ident;
        generics
            .make_where_clause()
            .predicates
            .push(parse_quote!(#ty: ::siderite::validation::Schema + 'static));
    }
    if hooks == Hooks::Direct {
        let (_, ty_generics, _) = input.generics.split_for_impl();
        generics
            .make_where_clause()
            .predicates
            .push(parse_quote!(for<'__a> #ident #ty_generics: ::siderite::validation::ModelHooks));
    }
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();
    let preamble = hooks.preamble();
    let dump = if container.options.dump == Some(false) {
        TokenStream::new()
    } else {
        dump::expand(input, &container, hooks)
    };
    Ok(quote! {
        const _: () = {
            #preamble

            impl #impl_generics ::siderite::validation::Schema for #ident #ty_generics #where_clause {
                fn schema_name() -> ::core::option::Option<&'static str> {
                    #name
                }

                #[allow(unused_variables, clippy::needless_borrow)]
                fn schema(registry: &mut ::siderite::validation::SchemaRegistry)
                    -> ::siderite::validation::SchemaObject
                {
                    #body
                }
            }

            #dump
        };
    })
}
