use quote::{quote, quote_spanned};
use syn::{parse::Parser, parse_macro_input, spanned::Spanned, Expr, ExprLit, Lit, Meta};

#[derive(Debug, Clone, Copy, Default)]
enum RuntimeFlavor {
    #[default]
    MultiThread,
    CurrentThread,
}

#[derive(Debug)]
enum RuntimeFlavorParseError {
    InvalidVariant(String),
}

impl std::fmt::Display for RuntimeFlavorParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidVariant(s) => {
                write!(
                    f,
                    "flavor must be \"multi_thread\" or \"current_thread\", got \"{s}\""
                )
            }
        }
    }
}

impl RuntimeFlavor {
    fn from_str(s: &str) -> Result<Self, RuntimeFlavorParseError> {
        match s {
            "multi_thread" => Ok(Self::MultiThread),
            "current_thread" => Ok(Self::CurrentThread),
            s => Err(RuntimeFlavorParseError::InvalidVariant(s.to_string())),
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
                                config.flavor = RuntimeFlavor::from_str(s.value().as_str())
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
pub fn gc_main(
    args: proc_macro::TokenStream,
    item: proc_macro::TokenStream,
) -> proc_macro::TokenStream {
    let input = parse_macro_input!(item as syn::ItemFn);

    let config = if args.is_empty() {
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
                let meta: Meta = nv.into();
                let mut punctuated = syn::punctuated::Punctuated::new();
                punctuated.push(meta);
                match GcMainConfig::from_args(&punctuated) {
                    Ok(config) => config,
                    Err(err) => return err.into_compile_error().into(),
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
