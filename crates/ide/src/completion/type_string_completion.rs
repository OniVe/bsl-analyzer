use std::collections::HashSet;

use bsl_platform::PlatformDataInner;
use ide_db::base_db::Locale;
use ide_db::RootDatabase;
use stdx::case::CaseExt;
use syntax::{SyntaxKind, SyntaxNode, TextSize};

use super::{CompletionItem, CompletionItemKind, CompletionPosition};

const PRIMITIVE_TYPE_NAMES_RU: [&str; 6] =
    ["Число", "Строка", "Дата", "Булево", "Неопределено", "Null"];
const PRIMITIVE_TYPE_NAMES_EN: [&str; 6] =
    ["Number", "String", "Date", "Boolean", "Undefined", "Null"];

/// Completion inside the string argument of `Тип("…")` / `Type("…")`: offers
/// platform type names (`ТаблицаЗначений`, `Структура`, …) and configuration
/// reference types (`СправочникСсылка.<Имя>`, `ДокументСсылка.<Имя>`, …).
///
/// Prefix-aware: once a dotted prefix is typed (`ДокументСсылка.`), the labels
/// become just the trailing segment (`Документ1`), so the client — which
/// filters/replaces by the word after the last `.` — narrows and inserts
/// correctly instead of listing everything.
///
/// The platform accepts type names in either language. Without a typed base the
/// list holds the names of the interface locale only; once a base is typed, both
/// languages are matched against it, so `DocumentRef.` narrows in a Russian
/// locale as well.
pub(super) fn type_string_completions<DB: RootDatabase>(
    db: &DB,
    position: CompletionPosition,
) -> Option<Vec<CompletionItem>> {
    let parse = db.parse(position.file_id);
    let root = parse.syntax_node();
    let token = root.token_at_offset(position.offset).left_biased()?;

    if !token.kind().is_string_literal() {
        return None;
    }

    let call = type_call_of_argument(&token.parent()?)?;
    if !callee_is_type(&call) {
        return None;
    }

    // Text typed inside the string up to the cursor (without the surrounding
    // quotes), and the base = everything up to and including the last dot.
    let tok_text = token.text();
    let tok_len = TextSize::of(tok_text);
    let rel = (position.offset - token.text_range().start()).min(tok_len);
    // Cursor right after the closing quote is outside the string.
    if rel == tok_len && tok_len > TextSize::from(1) && tok_text.ends_with('"') {
        return None;
    }
    // A continuation line follows a line break, which belongs to the string
    // value, so no type name can be completed there; matching only this line
    // would also re-insert the base typed on an earlier line.
    if matches!(token.kind(), SyntaxKind::STRING_PART | SyntaxKind::STRING_TAIL) {
        return Some(Vec::new());
    }
    let typed = tok_text
        .get(1..usize::from(rel)) // skip the opening quote
        .unwrap_or("")
        .trim_end_matches('"');
    let base_lower = match typed.rfind('.') {
        Some(i) => typed[..=i].fold_lower(),
        None => String::new(),
    };

    let locales: &[Locale] =
        if base_lower.is_empty() { &[position.locale] } else { &[Locale::Ru, Locale::En] };

    let mut items = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    // Primitive and special types are absent from the platform type list, yet
    // `Тип()` accepts them. They have no dotted form, so only without a base.
    if base_lower.is_empty() {
        let names = match position.locale {
            Locale::Ru => PRIMITIVE_TYPE_NAMES_RU,
            Locale::En => PRIMITIVE_TYPE_NAMES_EN,
        };
        for name in names {
            consider(&mut items, &mut seen, &base_lower, name);
        }
    }

    // Platform value types. Generic templated names such as
    // `ДокументСсылка.<Имя документа>` are skipped in `consider`: the concrete
    // forms come from the configuration below.
    for ty in PlatformDataInner::instance().all_types() {
        for &locale in locales {
            let name = match locale {
                Locale::Ru => ty.name.as_str(),
                Locale::En => ty.english_name.as_str(),
            };
            consider(&mut items, &mut seen, &base_lower, name);
        }
    }

    // Configuration reference types: `ref_kind_for` maps a metadata object kind
    // to its reference kind, whose label is the type prefix (`ДокументСсылка`,
    // `DocumentRef`, …).
    let configs = db.get_all_configurations(position.file_id);
    for (_source, config) in &configs {
        for mdo in config.metadata_objects() {
            if let Some(kind) = hir::MetadataKind::ref_kind_for(mdo.mdo_type) {
                for &locale in locales {
                    let prefix = kind.display_label(locale);
                    consider(
                        &mut items,
                        &mut seen,
                        &base_lower,
                        &format!("{}.{}", prefix, mdo.name),
                    );
                }
            }
        }
    }

    Some(items)
}

/// Add `name` as a candidate. Drops templated/placeholder names. When a dotted
/// `base` is already typed, only names under that base survive and the label is
/// the trailing segment (so the client matches the after-dot word).
fn consider(
    items: &mut Vec<CompletionItem>,
    seen: &mut HashSet<String>,
    base_lower: &str,
    name: &str,
) {
    // Placeholder / HTML-escaped template names (`...<Имя>`, `...&lt;Имя&gt;`).
    if name.contains('<') || name.contains('&') {
        return;
    }

    let (label, detail): (&str, String) = if base_lower.is_empty() {
        (name, "Тип".to_string())
    } else {
        if !name.fold_lower().starts_with(base_lower) {
            return;
        }
        // Case folding keeps the byte length for the Cyrillic/Latin names we
        // emit; `get` guards against any name where it would not.
        let Some(rest) = name.get(base_lower.len()..) else {
            return;
        };
        (rest, name.to_string())
    };

    if label.is_empty() || !seen.insert(label.fold_lower()) {
        return;
    }

    items.push(CompletionItem {
        label: label.to_string(),
        detail: Some(detail),
        kind: CompletionItemKind::MdoType,
        insert_text: label.to_string(),
        documentation: None,
        sort_text: None,
        filter_text: None,
        source: None,
    });
}

/// The call whose argument list holds the string literal `literal` as a whole
/// argument. Only `EXPR` wrappers may stand between them: a string inside a
/// concatenation, an index key or a nested call is not the type name.
fn type_call_of_argument(literal: &SyntaxNode) -> Option<SyntaxNode> {
    if literal.kind() != SyntaxKind::LITERAL {
        return None;
    }
    let arg_list = literal.ancestors().skip(1).find(|n| n.kind() != SyntaxKind::EXPR)?;
    if arg_list.kind() != SyntaxKind::ARG_LIST {
        return None;
    }
    arg_list.parent().filter(|call| call.kind() == SyntaxKind::CALL_EXPR)
}

/// `Тип` is a bare global function: the callee — the first child of the call —
/// must be a lone `IDENT` node, not a `FIELD_EXPR` such as `Тип.Вставить` on a
/// variable that happens to be named `Тип`. Matched case-insensitively in both
/// languages.
fn callee_is_type(call: &SyntaxNode) -> bool {
    call.children()
        .next()
        .filter(|callee| callee.kind() == SyntaxKind::IDENT)
        .and_then(|callee| callee.first_token())
        .is_some_and(|t| {
            t.kind() == SyntaxKind::IDENT
                && matches!(t.text().fold_lower().as_str(), "тип" | "type")
        })
}
