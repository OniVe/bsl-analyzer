mod bsl_completion;
pub(crate) mod env_filter;
mod fuzzy;
mod mdo_completion;
mod new_expr_completion;
mod platform_completion;
mod sdbl;
mod templates;
mod type_string_completion;

use ide_db::base_db::Locale;
use ide_db::RootDatabase;
use std::path::PathBuf;
use syntax::TextSize;
use vfs::FileId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionPosition {
    pub file_id: FileId,
    pub offset: TextSize,
    pub workspace_root: Option<PathBuf>,
    pub locale: Locale,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionItem {
    pub label: String,

    pub detail: Option<String>,

    pub kind: CompletionItemKind,

    pub insert_text: String,

    pub documentation: Option<String>,

    pub sort_text: Option<String>,

    pub filter_text: Option<String>,

    pub source: Option<String>,
}

impl CompletionItem {
    pub fn simple(label: String, kind: CompletionItemKind, insert_text: String) -> Self {
        Self {
            label,
            kind,
            detail: None,
            documentation: None,
            insert_text,
            sort_text: None,
            filter_text: None,
            source: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionItemKind {
    MdoType,
    MdoObject,
    Field,
    Property,
    Function,
    Method,
    Keyword,
    Constant,
    EnumMember,
    Constructor,
    Snippet,
}

pub fn completions<DB: RootDatabase>(db: &DB, position: CompletionPosition) -> Vec<CompletionItem> {
    let _p = tracing::info_span!("completions", ?position).entered();

    // Inside the string argument of `Тип("…")`. Checked first: it is disjoint from
    // the query-string and dotted-member contexts below.
    if let Some(items) = type_string_completion::type_string_completions(db, position.clone()) {
        tracing::debug!(items = items.len(), "returning Тип(\"…\") type-name completions");
        return items;
    }

    if let Some(items) = sdbl::sdbl_completions(db, position.clone()) {
        tracing::debug!(items = items.len(), "returning SDBL completions");
        return items;
    }

    if let Some(items) = new_expr_completion::new_expr_completions(db, position.clone()) {
        tracing::debug!(items = items.len(), "returning constructor completions after `Новый`");
        return items;
    }

    if let Some(items) = mdo_completion::mdo_completions(db, position.clone()) {
        tracing::debug!(items = items.len(), "returning MDO completions");
        return items;
    }

    if let Some(items) = platform_completion::platform_completions(db, position.clone()) {
        tracing::debug!(items = items.len(), "returning platform method completions");
        return items;
    }

    if let Some(items) = bsl_completion::bsl_completions(db, position.clone()) {
        tracing::debug!(items = items.len(), "returning BSL completions");
        return items;
    }

    tracing::trace!("no completions available");

    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_completion_item_creation() {
        let item = CompletionItem {
            label: "Валюты".to_string(),
            detail: Some("Справочник".to_string()),
            kind: CompletionItemKind::MdoObject,
            insert_text: "Валюты".to_string(),
            documentation: None,
            sort_text: None,
            filter_text: None,
            source: None,
        };

        assert_eq!(item.label, "Валюты");
        assert_eq!(item.detail, Some("Справочник".to_string()));
        assert_eq!(item.kind, CompletionItemKind::MdoObject);
    }

    #[test]
    fn test_completion_item_simple() {
        let item = CompletionItem::simple(
            "Test".to_string(),
            CompletionItemKind::Field,
            "Test".to_string(),
        );

        assert_eq!(item.label, "Test");
        assert_eq!(item.kind, CompletionItemKind::Field);
        assert_eq!(item.detail, None);
        assert_eq!(item.sort_text, None);
        assert_eq!(item.filter_text, None);
    }
}
