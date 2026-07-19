//! 编译并完善 Nihility gRPC 协议生成代码。

use std::path::PathBuf;
use syn::visit_mut::{self, VisitMut};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=proto/nsetup.proto");
    println!("cargo:rerun-if-changed=proto/stack.proto");
    tonic_prost_build::configure()
        .compile_protos(&["proto/nsetup.proto", "proto/stack.proto"], &["proto"])?;

    let generated = PathBuf::from(std::env::var("OUT_DIR")?).join("nsetup.v1.rs");
    let source = std::fs::read_to_string(&generated)?;
    let mut syntax = syn::parse_file(&source)?;
    GeneratedDocs.visit_file_mut(&mut syntax);
    std::fs::write(generated, prettyplease::unparse(&syntax))?;
    Ok(())
}

/// 为没有对应 proto 声明的 Tonic 私有实现补充文档。
struct GeneratedDocs;

impl VisitMut for GeneratedDocs {
    fn visit_field_mut(&mut self, field: &mut syn::Field) {
        ensure_doc(&mut field.attrs);
        visit_mut::visit_field_mut(self, field);
    }

    fn visit_variant_mut(&mut self, variant: &mut syn::Variant) {
        ensure_doc(&mut variant.attrs);
        visit_mut::visit_variant_mut(self, variant);
    }

    fn visit_item_const_mut(&mut self, item: &mut syn::ItemConst) {
        ensure_doc(&mut item.attrs);
        visit_mut::visit_item_const_mut(self, item);
    }

    fn visit_item_enum_mut(&mut self, item: &mut syn::ItemEnum) {
        ensure_doc(&mut item.attrs);
        visit_mut::visit_item_enum_mut(self, item);
    }

    fn visit_item_fn_mut(&mut self, item: &mut syn::ItemFn) {
        ensure_doc(&mut item.attrs);
        visit_mut::visit_item_fn_mut(self, item);
    }

    fn visit_item_mod_mut(&mut self, item: &mut syn::ItemMod) {
        ensure_doc(&mut item.attrs);
        visit_mut::visit_item_mod_mut(self, item);
    }

    fn visit_item_static_mut(&mut self, item: &mut syn::ItemStatic) {
        ensure_doc(&mut item.attrs);
        visit_mut::visit_item_static_mut(self, item);
    }

    fn visit_item_struct_mut(&mut self, item: &mut syn::ItemStruct) {
        ensure_doc(&mut item.attrs);
        visit_mut::visit_item_struct_mut(self, item);
    }

    fn visit_item_trait_mut(&mut self, item: &mut syn::ItemTrait) {
        ensure_doc(&mut item.attrs);
        visit_mut::visit_item_trait_mut(self, item);
    }

    fn visit_item_type_mut(&mut self, item: &mut syn::ItemType) {
        ensure_doc(&mut item.attrs);
        visit_mut::visit_item_type_mut(self, item);
    }

    fn visit_impl_item_const_mut(&mut self, item: &mut syn::ImplItemConst) {
        ensure_doc(&mut item.attrs);
        visit_mut::visit_impl_item_const_mut(self, item);
    }

    fn visit_impl_item_fn_mut(&mut self, item: &mut syn::ImplItemFn) {
        ensure_doc(&mut item.attrs);
        make_const_when_structurally_valid(item);
        visit_mut::visit_impl_item_fn_mut(self, item);
    }

    fn visit_impl_item_type_mut(&mut self, item: &mut syn::ImplItemType) {
        ensure_doc(&mut item.attrs);
        visit_mut::visit_impl_item_type_mut(self, item);
    }

    fn visit_trait_item_const_mut(&mut self, item: &mut syn::TraitItemConst) {
        ensure_doc(&mut item.attrs);
        visit_mut::visit_trait_item_const_mut(self, item);
    }

    fn visit_trait_item_fn_mut(&mut self, item: &mut syn::TraitItemFn) {
        ensure_doc(&mut item.attrs);
        visit_mut::visit_trait_item_fn_mut(self, item);
    }

    fn visit_trait_item_type_mut(&mut self, item: &mut syn::TraitItemType) {
        ensure_doc(&mut item.attrs);
        visit_mut::visit_trait_item_type_mut(self, item);
    }
}

/// 仅为缺少 proto 文档来源的生成项添加说明。
fn ensure_doc(attributes: &mut Vec<syn::Attribute>) {
    if !attributes
        .iter()
        .any(|attribute| attribute.path().is_ident("doc"))
    {
        attributes.push(syn::parse_quote!(
            #[doc = " Tonic 生成的内部传输实现。"]
        ));
    }

    for attribute in attributes {
        let syn::Meta::NameValue(meta) = &mut attribute.meta else {
            continue;
        };
        let syn::Expr::Lit(expression) = &mut meta.value else {
            continue;
        };
        let syn::Lit::Str(documentation) = &mut expression.lit else {
            continue;
        };
        let normalized = normalize_markdown_identifiers(&documentation.value());
        *documentation = syn::LitStr::new(&normalized, documentation.span());
    }
}

/// 将生成器文档中的复合标识符转换为 Markdown 代码样式。
fn normalize_markdown_identifiers(documentation: &str) -> String {
    let mut normalized = String::with_capacity(documentation.len());
    let mut word = String::new();
    let mut in_code = false;

    for character in documentation.chars() {
        if character == '`' {
            push_documentation_word(&mut normalized, &mut word, in_code);
            in_code = !in_code;
            normalized.push(character);
        } else if character.is_ascii_alphanumeric() {
            word.push(character);
        } else {
            push_documentation_word(&mut normalized, &mut word, in_code);
            normalized.push(character);
        }
    }
    push_documentation_word(&mut normalized, &mut word, in_code);
    normalized
}

/// 将一个文档单词写入最终文本。
fn push_documentation_word(output: &mut String, word: &mut String, in_code: bool) {
    let uppercase = word.chars().filter(char::is_ascii_uppercase).count();
    let has_lowercase = word.chars().any(|character| character.is_ascii_lowercase());
    let starts_lowercase = word
        .chars()
        .next()
        .is_some_and(|character| character.is_ascii_lowercase());
    let is_compound_identifier =
        has_lowercase && (uppercase > 1 || starts_lowercase && uppercase > 0);

    if is_compound_identifier && !in_code {
        output.push('`');
        output.push_str(word);
        output.push('`');
    } else {
        output.push_str(word);
    }
    word.clear();
}

/// 将仅包含可在常量上下文执行的生成方法声明为常量函数。
fn make_const_when_structurally_valid(item: &mut syn::ImplItemFn) {
    if item.sig.constness.is_some() || item.sig.asyncness.is_some() {
        return;
    }

    if is_match_accessor(item) || is_field_setter(item) {
        item.sig.constness = Some(syn::parse_quote!(const));
    }
}

/// 判断方法是否只是对借用接收者执行一次模式匹配。
fn is_match_accessor(item: &syn::ImplItemFn) -> bool {
    let Some(syn::FnArg::Receiver(receiver)) = item.sig.inputs.first() else {
        return false;
    };
    matches!(&receiver.kind, syn::ReceiverKind::Reference(..))
        && item.sig.inputs.len() == 1
        && matches!(
            item.block.stmts.as_slice(),
            [syn::Stmt::Expr(syn::Expr::Match(_), None)]
        )
}

/// 判断方法是否只更新自身字段并返回自身。
fn is_field_setter(item: &syn::ImplItemFn) -> bool {
    let Some(syn::FnArg::Receiver(receiver)) = item.sig.inputs.first() else {
        return false;
    };
    if matches!(&receiver.kind, syn::ReceiverKind::Reference(..)) || receiver.mutability.is_none() {
        return false;
    }

    let Some((last, assignments)) = item.block.stmts.split_last() else {
        return false;
    };
    is_self_expression(last)
        && !assignments.is_empty()
        && assignments.iter().all(is_self_field_assignment)
}

/// 判断语句是否返回 `self`。
fn is_self_expression(statement: &syn::Stmt) -> bool {
    matches!(
        statement,
        syn::Stmt::Expr(syn::Expr::Path(path), None)
            if path.path.is_ident("self")
    )
}

/// 判断语句是否给 `self` 的字段赋值。
fn is_self_field_assignment(statement: &syn::Stmt) -> bool {
    matches!(
        statement,
        syn::Stmt::Expr(
            syn::Expr::Assign(syn::ExprAssign { left, right, .. }),
            Some(_)
        ) if matches!(
            left.as_ref(),
            syn::Expr::Field(field)
                if matches!(field.base.as_ref(), syn::Expr::Path(path) if path.path.is_ident("self"))
        ) && is_const_assignment_value(right)
    )
}

/// 判断赋值右侧是否只包含常量构造。
fn is_const_assignment_value(expression: &syn::Expr) -> bool {
    match expression {
        syn::Expr::Lit(_) | syn::Expr::Path(_) => true,
        syn::Expr::Call(call) => {
            matches!(call.func.as_ref(), syn::Expr::Path(path) if path.path.is_ident("Some"))
                && call.args.iter().all(is_const_assignment_value)
        }
        _ => false,
    }
}
