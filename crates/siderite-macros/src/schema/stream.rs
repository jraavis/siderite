//! Direct serialization for ordinary derived models.

use crate::attrs::plan::FieldPlan;
use crate::probe::Hooks;
use proc_macro2::TokenStream;
use quote::quote;

/// Generate a streaming path, with value dumping for hooks and options.
pub fn named(plans: &[FieldPlan<'_>], hooks: Hooks) -> TokenStream {
    let hooked = match hooks {
        Hooks::Direct => return TokenStream::new(),
        Hooks::Off => quote!(false),
        Hooks::Probe => quote! {
            {
                let probe = &&::siderite::__private::Probe::<Self>::new();
                probe.has_hooks(self)
            }
        },
    };
    let fields: Vec<_> = plans
        .iter()
        .filter(|plan| plan.written && !plan.options.exclude)
        .collect();
    let mutable = (!fields.is_empty()).then(|| quote!(mut));
    let entries = fields.into_iter().map(|plan| {
        let ident = plan.ident;
        let key = &plan.key;
        let entry = quote! {
            __map.serialize_entry(
                #key,
                &::siderite::validation::dump::DumpSerialize(
                    &self.#ident, opts,
                ),
            )?;
        };
        match &plan.serde.skip_serializing_if {
            Some(skip) => quote!(if !#skip(&self.#ident) { #entry }),
            None => entry,
        }
    });
    quote! {
        fn serialize_dump<__S: ::siderite::__private::serde::Serializer>(
            &self,
            serializer: __S,
            opts: &::siderite::validation::DumpOptions,
        ) -> ::core::result::Result<__S::Ok, __S::Error> {
            use ::siderite::__private::serde::ser::SerializeMap as _;
            if #hooked || opts != &Default::default() {
                let value = ::siderite::validation::Dump::dump(self, opts)
                    .map_err(
                        ::siderite::__private::serde::ser::Error::custom,
                    )?;
                return ::siderite::__private::serde::Serialize::serialize(
                    &value, serializer,
                );
            }
            let #mutable __map = serializer.serialize_map(None)?;
            #(#entries)*
            __map.end()
        }
    }
}
