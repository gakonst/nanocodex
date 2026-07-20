use proc_macro::TokenStream;
use proc_macro_crate::{FoundCrate, crate_name};
use proc_macro2::Span;
use quote::{format_ident, quote};
use syn::{
    Error, FnArg, GenericArgument, ItemFn, LitStr, Pat, PathArguments, ReturnType, Token, Type,
    parse::Parser, punctuated::Punctuated,
};

struct ToolArgs {
    description: LitStr,
    name: Option<LitStr>,
}

/// Defines a JSON function tool from an async Rust function.
#[proc_macro_attribute]
pub fn tool(attributes: TokenStream, item: TokenStream) -> TokenStream {
    expand_tool(attributes.into(), item.into())
        .unwrap_or_else(Error::into_compile_error)
        .into()
}

fn expand_tool(
    attributes: proc_macro2::TokenStream,
    item: proc_macro2::TokenStream,
) -> syn::Result<proc_macro2::TokenStream> {
    let arguments = parse_args(attributes)?;
    let function = syn::parse2::<ItemFn>(item)?;
    validate_signature(&function)?;
    let output = result_output(&function.sig.output)?;
    let original_ident = &function.sig.ident;
    let handler_ident = format_ident!("__nanocodex_tool_{original_ident}");
    let input_ident = format_ident!("__NanocodexTool{}Input", pascal_case(original_ident));
    let visibility = &function.vis;
    let description = arguments.description;
    let tool_name = arguments
        .name
        .unwrap_or_else(|| LitStr::new(&original_ident.to_string(), original_ident.span()));
    let (nanocodex, nanocodex_path) = nanocodex_path();
    let serde_path = LitStr::new(
        &format!("{nanocodex_path}::__private::serde"),
        Span::call_site(),
    );
    let schemars_path = LitStr::new(
        &format!("{nanocodex_path}::__private::schemars"),
        Span::call_site(),
    );

    let (fields, calls) = function_arguments(&function)?;

    let mut hidden_function = function.clone();
    hidden_function.sig.ident = handler_ident.clone();
    hidden_function.vis = syn::Visibility::Inherited;
    let attrs = &function.attrs;

    Ok(quote! {
        #[allow(clippy::unused_async)]
        #hidden_function

        #(#attrs)*
        #[allow(non_camel_case_types)]
        #visibility struct #original_ident;

        #[derive(
            #nanocodex::__private::serde::Deserialize,
            #nanocodex::__private::schemars::JsonSchema
        )]
        #[serde(
            crate = #serde_path,
            deny_unknown_fields
        )]
        #[schemars(crate = #schemars_path)]
        struct #input_ident {
            #(#fields,)*
        }

        #[#nanocodex::__private::async_trait]
        impl #nanocodex::Tool for #original_ident {
            fn name(&self) -> &'static str {
                #tool_name
            }

            fn definition(&self) -> #nanocodex::ToolDefinition {
                #nanocodex::ToolDefinition::function(
                    #tool_name,
                    #description,
                    #nanocodex::__private::schema_for::<#input_ident>(),
                )
                .with_output_schema(#nanocodex::__private::schema_for::<#output>())
            }

            async fn execute(
                &self,
                input: #nanocodex::ToolInput,
                _context: #nanocodex::ToolContext<'_>,
            ) -> #nanocodex::ToolResult {
                let input = input.decode_json::<#input_ident>()?;
                match #handler_ident(#(#calls),*).await {
                    Ok(output) => Ok(#nanocodex::ToolExecution::json(&output)),
                    Err(error) => Err(
                        ::std::io::Error::other(error.to_string()).into()
                    ),
                }
            }
        }
    })
}

fn validate_signature(function: &ItemFn) -> syn::Result<()> {
    if function.sig.asyncness.is_none() {
        return Err(Error::new_spanned(
            function.sig.fn_token,
            "#[tool] requires an async function",
        ));
    }
    if !function.sig.generics.params.is_empty() {
        return Err(Error::new_spanned(
            &function.sig.generics,
            "#[tool] functions cannot be generic",
        ));
    }
    Ok(())
}

fn function_arguments(
    function: &ItemFn,
) -> syn::Result<(Vec<proc_macro2::TokenStream>, Vec<proc_macro2::TokenStream>)> {
    let mut fields = Vec::with_capacity(function.sig.inputs.len());
    let mut calls = Vec::with_capacity(function.sig.inputs.len());
    for argument in &function.sig.inputs {
        let FnArg::Typed(argument) = argument else {
            return Err(Error::new_spanned(
                argument,
                "#[tool] does not support receivers",
            ));
        };
        let Pat::Ident(pattern) = argument.pat.as_ref() else {
            return Err(Error::new_spanned(
                &argument.pat,
                "#[tool] arguments must use simple identifier patterns",
            ));
        };
        let ident = &pattern.ident;
        let ty = &argument.ty;
        fields.push(quote!(#ident: #ty));
        calls.push(quote!(input.#ident));
    }
    Ok((fields, calls))
}

fn parse_args(attributes: proc_macro2::TokenStream) -> syn::Result<ToolArgs> {
    let entries =
        Punctuated::<syn::MetaNameValue, Token![,]>::parse_terminated.parse2(attributes)?;
    let mut description = None;
    let mut name = None;
    for entry in entries {
        let Some(ident) = entry.path.get_ident() else {
            return Err(Error::new_spanned(
                entry.path,
                "unsupported #[tool] argument",
            ));
        };
        let syn::Expr::Lit(expression) = entry.value else {
            return Err(Error::new_spanned(entry.value, "expected a string literal"));
        };
        let syn::Lit::Str(value) = expression.lit else {
            return Err(Error::new_spanned(
                expression.lit,
                "expected a string literal",
            ));
        };
        match ident.to_string().as_str() {
            "description" if description.is_none() => description = Some(value),
            "name" if name.is_none() => name = Some(value),
            "description" | "name" => {
                return Err(Error::new_spanned(ident, "duplicate #[tool] argument"));
            }
            _ => return Err(Error::new_spanned(ident, "unsupported #[tool] argument")),
        }
    }
    let Some(description) = description else {
        return Err(Error::new(
            Span::call_site(),
            "#[tool] requires description = \"...\"",
        ));
    };
    Ok(ToolArgs { description, name })
}

fn result_output(output: &ReturnType) -> syn::Result<&Type> {
    let ReturnType::Type(_, ty) = output else {
        return Err(Error::new_spanned(
            output,
            "#[tool] functions must return Result<T, E>",
        ));
    };
    let Type::Path(path) = ty.as_ref() else {
        return Err(Error::new_spanned(
            ty,
            "#[tool] functions must return Result<T, E>",
        ));
    };
    let Some(segment) = path.path.segments.last() else {
        return Err(Error::new_spanned(ty, "expected Result<T, E>"));
    };
    if segment.ident != "Result" {
        return Err(Error::new_spanned(ty, "expected Result<T, E>"));
    }
    let PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return Err(Error::new_spanned(ty, "expected Result<T, E>"));
    };
    arguments
        .args
        .iter()
        .find_map(|argument| match argument {
            GenericArgument::Type(ty) => Some(ty),
            _ => None,
        })
        .ok_or_else(|| Error::new_spanned(ty, "expected Result<T, E>"))
}

fn nanocodex_path() -> (proc_macro2::TokenStream, String) {
    match crate_name("nanocodex") {
        Ok(FoundCrate::Itself) | Err(_) => (quote!(::nanocodex), "::nanocodex".to_owned()),
        Ok(FoundCrate::Name(name)) => {
            let ident = syn::Ident::new(&name, Span::call_site());
            (quote!(::#ident), format!("::{name}"))
        }
    }
}

fn pascal_case(ident: &syn::Ident) -> String {
    ident
        .to_string()
        .split('_')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut characters = part.chars();
            characters.next().map_or_else(String::new, |first| {
                first.to_uppercase().chain(characters).collect()
            })
        })
        .collect()
}
