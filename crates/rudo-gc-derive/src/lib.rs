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
pub fn main(
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

/// Derive macro for the `Trace` trait.
///
/// This macro generates implementations of the `Trace` trait for structs and enums.
/// The generated implementation calls `trace()` on each field recursively.
///
/// # Write Barrier Compatibility
///
/// The derived `Trace` implementation is compatible with incremental marking write barriers.
/// When incremental marking is active, mutations to `Gc<T>` fields are tracked via the
/// `GcCell<T>` write barrier, ensuring correctness under concurrent mutation.
///
/// For types that need custom write barrier behavior, implement `Trace` manually.
///
/// # Example
///
/// ```rust
/// use rudo_gc::Trace;
///
/// #[derive(Trace)]
/// struct MyStruct {
///     field1: Gc<OtherStruct>,
///     field2: Vec<Gc<AnotherStruct>>,
/// }
/// ```
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

/// Derive macro for `GcCell` compatibility.
///
/// This macro automatically implements `GcCapture` for types containing `Gc<T>` fields,
/// enabling SATB barrier recording during incremental marking.
///
/// # Usage
///
/// ```rust
/// use rudo_gc::{Gc, Trace, cell::GcCell};
///
/// #[derive(Trace, GcCell)]
/// struct MyStruct {
///     gc_field: Gc<Other>,      // Auto-implements GcCapture
///     regular_field: i32,        // No GcCapture needed
/// }
/// ```
///
/// # What It Generates
///
/// For types containing `Gc<T>` fields, the macro generates:
/// ```rust
/// unsafe impl GcCapture for MyStruct {
///     fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
///         self.gc_field.capture_gc_ptrs_into(ptrs);
///     }
/// }
/// ```
///
/// For types without `Gc<T>` fields, no impl is generated (no SATB barrier needed).
///
/// # Limitations
///
/// - Enums: Not supported (use manual implementation)
/// - Generic types: Not supported (use manual implementation)
/// - Recursive types: Not supported (use manual implementation)
///
/// # Example
///
/// ```
/// use rudo_gc::{Gc, Trace, cell::GcCell};
///
/// #[derive(Trace, GcCell)]
/// struct Node {
///     value: i32,
///     next: GcCell<Option<Gc<Node>>>,
/// }
///
/// fn example() {
///     let cell = GcCell::new(Node {
///         value: 42,
///         next: GcCell::new(None),
///     });
///     *cell.borrow_mut() = Node {
///         value: 100,
///         next: GcCell::new(None),
///     };
/// }
/// ```
#[proc_macro_derive(GcCell, attributes(rudo_gc))]
pub fn derive_gc_cell(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
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

    let gc_fields = match &input.data {
        Data::Struct(struct_data) => analyze_struct_fields(&struct_data.fields),
        _ => Vec::new(),
    };

    let generics = add_gc_capture_bounds(&rudo_gc, input.generics, &gc_fields);
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

    match &input.data {
        Data::Struct(_struct_data) => {
            if gc_fields.is_empty() {
                let gc_capture_body = generate_empty_gc_capture_body(&rudo_gc);
                let expanded = quote! {
                    impl #impl_generics #rudo_gc::cell::GcCapture
                        for #name #ty_generics #where_clause {
                        #gc_capture_body
                    }
                };
                return expanded.into();
            }

            let gc_capture_body = generate_gc_capture_body(&rudo_gc, &gc_fields);

            let expanded = quote! {
                impl #impl_generics #rudo_gc::cell::GcCapture
                    for #name #ty_generics #where_clause {
                    #[inline]
                    fn capture_gc_ptrs(&self) -> &[std::ptr::NonNull<#rudo_gc::GcBox<()>>] {
                        &[]
                    }

                    #[inline]
                    fn capture_gc_ptrs_into(
                        &self,
                        ptrs: &mut Vec<std::ptr::NonNull<#rudo_gc::GcBox<()>>>
                    ) {
                        #gc_capture_body
                    }
                }
            };
            expanded.into()
        }
        Data::Enum(_) => syn::Error::new_spanned(
            name,
            "GcCell derive does not support enums. Use manual implementation.",
        )
        .into_compile_error()
        .into(),
        Data::Union(_) => syn::Error::new_spanned(name, "GcCell derive does not support unions.")
            .into_compile_error()
            .into(),
    }
}

/// Analyzes struct fields and returns those that contain `Gc<T>`.
fn analyze_struct_fields(fields: &syn::Fields) -> Vec<FieldInfo<'_>> {
    match fields {
        syn::Fields::Named(named) => {
            let mut gc_fields = Vec::new();
            for field in &named.named {
                if field_contains_gc(&field.ty) {
                    gc_fields.push(FieldInfo {
                        ident: field.ident.clone(),
                        index: None,
                        ty: &field.ty,
                    });
                }
            }
            gc_fields
        }
        syn::Fields::Unnamed(unnamed) => {
            let mut gc_fields = Vec::new();
            for (i, field) in unnamed.unnamed.iter().enumerate() {
                if field_contains_gc(&field.ty) {
                    gc_fields.push(FieldInfo {
                        ident: Some(format_ident!("field_{}", i)),
                        index: Some(i),
                        ty: &field.ty,
                    });
                }
            }
            gc_fields
        }
        syn::Fields::Unit => Vec::new(),
    }
}

/// Field information for code generation.
struct FieldInfo<'a> {
    ident: Option<syn::Ident>,
    index: Option<usize>,
    #[allow(dead_code)]
    ty: &'a syn::Type,
}

/// Checks if a type contains `Gc<T>`.
fn field_contains_gc(ty: &syn::Type) -> bool {
    match ty {
        syn::Type::Path(syn::TypePath { qself: None, path }) => {
            // Check if the path is `Gc` or `Gc<T>` (including fully qualified paths like ::Gc or module::Gc)
            if let Some(first_seg) = path.segments.first() {
                if first_seg.ident == "Gc" {
                    return true;
                }
            }

            // Check for generic types like Vec<Gc<T>>, Option<Gc<T>>, GcCell<Gc<T>>
            if let Some(last_seg) = path.segments.last() {
                if last_seg.ident == "Vec"
                    || last_seg.ident == "Option"
                    || last_seg.ident == "GcCell"
                {
                    if let syn::PathArguments::AngleBracketed(args) = &last_seg.arguments {
                        for arg in &args.args {
                            if let syn::GenericArgument::Type(inner_ty) = arg {
                                if field_contains_gc(inner_ty) {
                                    return true;
                                }
                            }
                        }
                    }
                }
            }

            if let Some(seg) = path.segments.last() {
                if seg.ident == "Gc"
                    || seg.ident == "Vec"
                    || seg.ident == "Option"
                    || seg.ident == "GcCell"
                {
                    if let syn::PathArguments::AngleBracketed(args) = &seg.arguments {
                        for arg in &args.args {
                            if let syn::GenericArgument::Type(inner_ty) = arg {
                                if field_contains_gc(inner_ty) {
                                    return true;
                                }
                            }
                        }
                    }
                }
            }

            false
        }
        syn::Type::Array(syn::TypeArray { elem, .. }) => {
            // Check [T; N] where T is Gc<T>
            field_contains_gc(elem)
        }
        _ => false,
    }
}

/// Generates the body for `GcCapture::capture_gc_ptrs_into`.
fn generate_gc_capture_body(rudo_gc: &syn::Path, fields: &[FieldInfo]) -> TokenStream {
    let calls: Vec<TokenStream> = fields
        .iter()
        .map(|field| {
            let field_access = match (field.index, &field.ident) {
                (Some(index), _) => {
                    let idx = Index::from(index);
                    quote! { self.#idx }
                }
                (None, Some(ident)) => quote! { self.#ident },
                (None, None) => panic!("FieldInfo must have either ident or index"),
            };
            quote! {
                #rudo_gc::cell::GcCapture::capture_gc_ptrs_into(&#field_access, ptrs);
            }
        })
        .collect();

    quote! {
        #(#calls)*
    }
}

/// Generates an empty `GcCapture` impl for types without Gc<T> fields.
fn generate_empty_gc_capture_body(rudo_gc: &syn::Path) -> TokenStream {
    quote! {
        #[inline]
        fn capture_gc_ptrs(&self) -> &[std::ptr::NonNull<#rudo_gc::GcBox<()>>] {
            &[]
        }

        #[inline]
        fn capture_gc_ptrs_into(&self, _ptrs: &mut Vec<std::ptr::NonNull<#rudo_gc::GcBox<()>>>) {
            // No Gc<T> fields - nothing to capture
        }
    }
}

/// Checks if a type contains a specific type parameter.
fn type_contains_param(ty: &syn::Type, target_param: &syn::Ident) -> bool {
    match ty {
        syn::Type::Path(syn::TypePath { qself: None, path }) => {
            if path.segments.len() == 1 && path.segments[0].ident == *target_param {
                return true;
            }
            for seg in &path.segments {
                if let syn::PathArguments::AngleBracketed(args) = &seg.arguments {
                    for arg in &args.args {
                        if let syn::GenericArgument::Type(inner_ty) = arg {
                            if type_contains_param(inner_ty, target_param) {
                                return true;
                            }
                        }
                    }
                }
            }
            false
        }
        syn::Type::Array(syn::TypeArray { elem, .. })
        | syn::Type::Slice(syn::TypeSlice { elem, .. }) => type_contains_param(elem, target_param),
        syn::Type::Tuple(syn::TypeTuple { elems, .. }) => {
            elems.iter().any(|t| type_contains_param(t, target_param))
        }
        syn::Type::Reference(syn::TypeReference { elem, .. }) => {
            type_contains_param(elem, target_param)
        }
        syn::Type::BareFn(syn::TypeBareFn { inputs, output, .. }) => {
            inputs
                .iter()
                .any(|arg| type_contains_param(&arg.ty, target_param))
                || match output {
                    syn::ReturnType::Default => false,
                    syn::ReturnType::Type(_, ty) => type_contains_param(ty, target_param),
                }
        }
        _ => false,
    }
}

/// Adds `GcCapture` bounds to type parameters that appear in Gc-containing fields.
fn add_gc_capture_bounds(
    rudo_gc: &Path,
    mut generics: Generics,
    gc_fields: &[FieldInfo],
) -> Generics {
    for param in &mut generics.params {
        if let GenericParam::Type(ref mut type_param) = *param {
            let needs_gc_capture = gc_fields
                .iter()
                .any(|field| type_contains_param(field.ty, &type_param.ident));

            if needs_gc_capture {
                let has_gc_capture = type_param.bounds.iter().any(|b| {
                    if let syn::TypeParamBound::Trait(t) = b {
                        t.path
                            .segments
                            .last()
                            .is_some_and(|s| s.ident == "GcCapture")
                    } else {
                        false
                    }
                });

                if !has_gc_capture {
                    type_param
                        .bounds
                        .push(parse_quote!(#rudo_gc::cell::GcCapture));
                }
            }
        }
    }
    generics
}
