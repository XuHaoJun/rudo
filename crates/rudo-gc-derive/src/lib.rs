//! Derive macro for the `Trace` trait.

use proc_macro2::TokenStream;
use quote::{format_ident, quote, quote_spanned};
use syn::{
    parse::Parser, parse_macro_input, parse_quote, spanned::Spanned, Data, DeriveInput, Expr,
    ExprLit, Fields, GenericParam, Generics, Ident, Index, Lit, Meta, Path,
};

#[derive(Debug, Clone, Copy, Default)]
enum RuntimeFlavor {
    #[default]
    MultiThread,
    CurrentThread,
}

impl RuntimeFlavor {
    fn from_string(s: &str) -> Result<Self, syn::Error> {
        match s {
            "multi_thread" => Ok(Self::MultiThread),
            "current_thread" => Ok(Self::CurrentThread),
            _ => Err(syn::Error::new(
                proc_macro2::Span::call_site(),
                "flavor must be \"multi_thread\" or \"current_thread\"",
            )),
        }
    }
}

#[derive(Debug)]
struct GcMainConfig {
    flavor: RuntimeFlavor,
    worker_threads: Option<u16>,
}

impl Default for GcMainConfig {
    fn default() -> Self {
        Self {
            flavor: RuntimeFlavor::MultiThread,
            worker_threads: None,
        }
    }
}

impl GcMainConfig {
    fn from_args(
        args: &syn::punctuated::Punctuated<Meta, syn::Token![,]>,
    ) -> Result<Self, syn::Error> {
        let mut config = Self::default();

        for arg in args {
            match arg {
                Meta::NameValue(nv) => {
                    let ident = nv
                        .path
                        .get_ident()
                        .ok_or_else(|| syn::Error::new_spanned(&nv.path, "expected ident"))?;

                    match ident.to_string().as_str() {
                        "flavor" => {
                            let value = &nv.value;
                            if let Expr::Lit(ExprLit {
                                lit: Lit::Str(s), ..
                            }) = value
                            {
                                config.flavor = RuntimeFlavor::from_string(s.value().as_str())
                                    .map_err(|e| syn::Error::new_spanned(s, e))?;
                            } else {
                                return Err(syn::Error::new_spanned(
                                    value,
                                    "flavor must be a string literal",
                                ));
                            }
                        }
                        "worker_threads" => {
                            let value = &nv.value;
                            if let Expr::Lit(ExprLit {
                                lit: Lit::Int(i), ..
                            }) = value
                            {
                                config.worker_threads = Some(i.base10_parse()?);
                            } else {
                                return Err(syn::Error::new_spanned(
                                    value,
                                    "worker_threads must be an integer literal",
                                ));
                            }
                        }
                        _ => {
                            return Err(syn::Error::new_spanned(
                                ident,
                                format!("unknown attribute: {ident}"),
                            ))
                        }
                    }
                }
                Meta::Path(path) => {
                    return Err(syn::Error::new_spanned(path, "expected key = value"))
                }
                Meta::List(list) => {
                    return Err(syn::Error::new_spanned(list, "expected key = value"))
                }
            }
        }

        Ok(config)
    }
}

#[proc_macro_attribute]
#[allow(clippy::too_many_lines)]
pub fn main(
    args: proc_macro::TokenStream,
    item: proc_macro::TokenStream,
) -> proc_macro::TokenStream {
    let input = parse_macro_input!(item as syn::ItemFn);

    let config =
        if args.is_empty() {
            GcMainConfig::default()
        } else {
            match parse_macro_input!(args as Meta) {
                Meta::List(list) => {
                    let inner =
                        match syn::punctuated::Punctuated::<Meta, syn::Token![,]>::parse_terminated
                            .parse2(list.tokens.clone())
                        {
                            Ok(inner) => inner,
                            Err(e) => {
                                return syn::Error::new_spanned(&list, e)
                                    .into_compile_error()
                                    .into()
                            }
                        };
                    match GcMainConfig::from_args(&inner) {
                        Ok(config) => config,
                        Err(err) => return err.into_compile_error().into(),
                    }
                }
                Meta::NameValue(nv) => {
                    let ident = nv
                        .path
                        .get_ident()
                        .ok_or_else(|| syn::Error::new_spanned(&nv.path, "expected ident"));
                    let ident = match ident {
                        Ok(ident) => ident,
                        Err(err) => return err.into_compile_error().into(),
                    };
                    match ident.to_string().as_str() {
                        "flavor" => {
                            if let Expr::Lit(ExprLit {
                                lit: Lit::Str(s), ..
                            }) = &nv.value
                            {
                                GcMainConfig {
                                    flavor: match s.value().as_str() {
                                        "multi_thread" => RuntimeFlavor::MultiThread,
                                        "current_thread" => RuntimeFlavor::CurrentThread,
                                        _ => return syn::Error::new_spanned(
                                            s,
                                            "flavor must be \"multi_thread\" or \"current_thread\"",
                                        )
                                        .into_compile_error()
                                        .into(),
                                    },
                                    worker_threads: None,
                                }
                            } else {
                                return syn::Error::new_spanned(
                                    &nv.value,
                                    "flavor must be a string literal",
                                )
                                .into_compile_error()
                                .into();
                            }
                        }
                        "worker_threads" => {
                            if let Expr::Lit(ExprLit {
                                lit: Lit::Int(i), ..
                            }) = &nv.value
                            {
                                let worker_threads = i
                                    .base10_parse()
                                    .map_err(|_| syn::Error::new_spanned(i, "invalid integer"));
                                let worker_threads = match worker_threads {
                                    Ok(n) => n,
                                    Err(err) => return err.into_compile_error().into(),
                                };
                                GcMainConfig {
                                    flavor: RuntimeFlavor::MultiThread,
                                    worker_threads: Some(worker_threads),
                                }
                            } else {
                                return syn::Error::new_spanned(
                                    &nv.value,
                                    "worker_threads must be an integer literal",
                                )
                                .into_compile_error()
                                .into();
                            }
                        }
                        _ => {
                            return syn::Error::new_spanned(
                                ident,
                                format!("unknown attribute: {ident}"),
                            )
                            .into_compile_error()
                            .into();
                        }
                    }
                }
                Meta::Path(path) => {
                    if path.is_ident("gc_main") || path.is_ident("main") {
                        GcMainConfig::default()
                    } else {
                        return syn::Error::new_spanned(
                            path,
                            "#[gc::main] requires parentheses: #[gc::main(...)]",
                        )
                        .into_compile_error()
                        .into();
                    }
                }
            }
        };

    if input.sig.asyncness.is_none() {
        return quote_spanned! {
            input.sig.fn_token.span =>
            compile_error!("the `async` keyword is missing from the function declaration");
        }
        .into();
    }

    let inputs = &input.sig.inputs;
    let has_params = !inputs.is_empty();

    if has_params && input.sig.ident != "main" {
        return quote_spanned! {
            input.sig.inputs.span() =>
            compile_error!("functions with #[gc::main] cannot have arguments");
        }
        .into();
    }

    let body = &input.block;
    let inputs = &input.sig.inputs;
    let ident = &input.sig.ident;
    let vis = &input.vis;
    let constness = input.sig.constness;
    let unsafety = input.sig.unsafety;
    let _abi = input.sig.abi.as_ref();

    let runtime_setup = match config.flavor {
        RuntimeFlavor::MultiThread => {
            let worker_threads = config
                .worker_threads
                .map_or_else(|| quote! {}, |n| quote! { .worker_threads(#n) });
            quote_spanned! {input.sig.span() =>
                ::tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    #worker_threads
            }
        }
        RuntimeFlavor::CurrentThread => {
            quote_spanned! {input.sig.span() =>
                ::tokio::runtime::Builder::new_current_thread()
                    .enable_all()
            }
        }
    };

    let expanded = if has_params {
        quote! {
            #vis #constness #unsafety fn #ident #inputs {
                let _ = ::rudo_gc::tokio::GcRootSet::global();

                let rt = #runtime_setup
                    .build()
                    .expect("Failed building the Runtime");

                rt.block_on(async #body)
            }
        }
    } else {
        quote! {
            #vis #constness #unsafety fn #ident () {
                let _ = ::rudo_gc::tokio::GcRootSet::global();

                let rt = #runtime_setup
                    .build()
                    .expect("Failed building the Runtime");

                rt.block_on(async #body)
            }
        }
    };

    expanded.into()
}

#[proc_macro_derive(Trace, attributes(rudo_gc))]
pub fn derive_trace(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let mut rudo_gc: Path = parse_quote!(::rudo_gc);

    for attr in &input.attrs {
        if !attr.path().is_ident("rudo_gc") {
            continue;
        }

        let result = attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("crate") {
                rudo_gc = meta.value()?.parse()?;
                Ok(())
            } else {
                Err(meta.error("unsupported attribute"))
            }
        });

        if let Err(err) = result {
            return err.into_compile_error().into();
        }
    }

    let name = &input.ident;
    let generics = add_trait_bounds(&rudo_gc, input.generics);
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();
    let trace_body = generate_trace_body(&rudo_gc, name, &input.data);

    let generated = quote! {
        unsafe impl #impl_generics #rudo_gc::Trace for #name #ty_generics #where_clause {
            #[inline]
            fn trace(&self, visitor: &mut impl #rudo_gc::Visitor) {
                #trace_body
            }
        }
    };

    generated.into()
}

fn add_trait_bounds(rudo_gc: &Path, mut generics: Generics) -> Generics {
    for param in &mut generics.params {
        if let GenericParam::Type(ref mut type_param) = *param {
            let has_trace = type_param.bounds.iter().any(|b| {
                if let syn::TypeParamBound::Trait(t) = b {
                    t.path.segments.last().is_some_and(|s| s.ident == "Trace")
                } else {
                    false
                }
            });
            let has_static = type_param.bounds.iter().any(|b| {
                if let syn::TypeParamBound::Lifetime(l) = b {
                    l.ident == "static"
                } else {
                    false
                }
            });

            if !has_trace {
                type_param.bounds.push(parse_quote!(#rudo_gc::Trace));
            }
            if !has_static {
                type_param.bounds.push(parse_quote!('static));
            }
        }
    }
    generics
}

fn generate_trace_body(rudo_gc: &Path, name: &Ident, data: &Data) -> TokenStream {
    match data {
        Data::Struct(data) => generate_struct_trace(rudo_gc, &data.fields),
        Data::Enum(data) => generate_enum_trace(rudo_gc, name, data),
        Data::Union(u) => {
            quote_spanned! {
                u.union_token.span => compile_error!("`Trace` must be manually implemented for unions");
            }
        }
    }
}

fn generate_struct_trace(rudo_gc: &Path, fields: &Fields) -> TokenStream {
    match fields {
        Fields::Named(f) => {
            let trace_calls = f.named.iter().map(|field| {
                let name = &field.ident;
                quote_spanned! {field.span() =>
                    #rudo_gc::Trace::trace(&self.#name, visitor);
                }
            });
            quote! { #(#trace_calls)* }
        }
        Fields::Unnamed(f) => {
            let trace_calls = f.unnamed.iter().enumerate().map(|(i, field)| {
                let index = Index::from(i);
                quote_spanned! {field.span() =>
                    #rudo_gc::Trace::trace(&self.#index, visitor);
                }
            });
            quote! { #(#trace_calls)* }
        }
        Fields::Unit => quote! {},
    }
}

fn generate_enum_trace(rudo_gc: &Path, name: &Ident, data: &syn::DataEnum) -> TokenStream {
    let match_arms = data.variants.iter().map(|variant| {
        let var_name = &variant.ident;
        match &variant.fields {
            Fields::Named(f) => {
                let field_names: Vec<_> = f
                    .named
                    .iter()
                    .enumerate()
                    .map(|(i, _)| format_ident!("field{}", i))
                    .collect();
                let field_idents: Vec<_> =
                    f.named.iter().map(|f| f.ident.as_ref().unwrap()).collect();
                let trace_calls = field_names.iter().map(|field| {
                    quote! { #rudo_gc::Trace::trace(#field, visitor); }
                });

                quote! {
                    #name::#var_name { #(#field_idents: #field_names),* } => {
                        #(#trace_calls)*
                    }
                }
            }
            Fields::Unnamed(f) => {
                let field_names: Vec<_> = (0..f.unnamed.len())
                    .map(|i| format_ident!("field{}", i))
                    .collect();
                let trace_calls = field_names.iter().map(|field| {
                    quote! { #rudo_gc::Trace::trace(#field, visitor); }
                });

                quote! {
                    #name::#var_name(#(#field_names),*) => {
                        #(#trace_calls)*
                    }
                }
            }
            Fields::Unit => {
                quote! {
                    #name::#var_name => {}
                }
            }
        }
    });

    quote! {
        match self {
            #(#match_arms)*
        }
    }
}
