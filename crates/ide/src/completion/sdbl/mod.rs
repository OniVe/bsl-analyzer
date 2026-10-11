pub mod domain;
pub mod infrastructure;
pub mod use_cases;

use super::{CompletionItem, CompletionPosition};
use domain::ScopeProvider;
use ide_db::RootDatabase;
use infrastructure::{DbMetadataProvider, DbScopeProvider};
use sdbl_hir::{detect_context, detect_sdbl_at_position, SdblCompletionContext};
use use_cases::{
    CompleteAliasesUseCase, CompleteCastFieldsUseCase, CompleteFieldsUseCase,
    CompleteJoinTypesUseCase, CompleteKeywordsUseCase, CompleteMdoUseCase,
    CompleteNestedElementsUseCase, CompleteNestedFieldsUseCase, CompleteValueElementsUseCase,
};

pub(super) fn sdbl_completions(
    db: &dyn RootDatabase,
    position: CompletionPosition,
) -> Option<Vec<CompletionItem>> {
    let file_id = position.file_id;
    let offset = position.offset;

    tracing::info!("sdbl_completions called: file_id={:?}, offset={:?}", file_id, offset);

    let parse = db.parse(file_id);
    let root = parse.syntax_node();

    let query_info = detect_sdbl_at_position(&root, offset)?;

    tracing::info!(
        query_len = query_info.query_text.len(),
        offset_in_query = u32::from(query_info.offset_in_query),
        "detected SDBL query"
    );

    let metadata_provider = DbMetadataProvider::new(db, file_id);
    let scope_provider = DbScopeProvider::new(db);

    let context = detect_context(&query_info.query_text, query_info.offset_in_query);

    // Only alias/nested completion needs a table Scope; its reference navigation
    // shares the file-scoped per-MDO resolver used by SDBL lowering.
    let needs_scope = matches!(
        context,
        SdblCompletionContext::AfterTableAlias { .. }
            | SdblCompletionContext::AfterNestedField { .. }
            | SdblCompletionContext::AfterOnKeyword { .. }
            | SdblCompletionContext::AfterCastExpression { .. }
    );
    let resolver = hir::DbSdblResolver::new(db, file_id);
    let scope = needs_scope
        .then(|| {
            scope_provider.get_scope(
                file_id,
                query_info.bsl_literal_range,
                query_info.offset_in_query,
                &resolver,
            )
        })
        .flatten();

    let items = match (context, scope.as_ref()) {
        (SdblCompletionContext::AfterTableAlias { alias, prefix }, Some(scope)) => {
            tracing::info!(
                alias = %alias,
                prefix = %prefix,
                "completion context: AfterTableAlias (with scope)"
            );
            CompleteFieldsUseCase::execute(scope, &alias, &prefix)
        }
        (SdblCompletionContext::AfterTableAlias { alias, prefix }, None) => {
            tracing::warn!(
                alias = %alias,
                prefix = %prefix,
                "completion context: AfterTableAlias but no scope available (HIR failed?)"
            );
            CompleteKeywordsUseCase::execute(&prefix)
        }

        (SdblCompletionContext::AfterNestedField { alias, field_chain, prefix }, Some(scope)) => {
            tracing::info!(
                alias = %alias,
                field_chain_len = field_chain.len(),
                prefix = %prefix,
                "completion context: AfterNestedField (with scope)"
            );
            CompleteNestedFieldsUseCase::execute(scope, &alias, &field_chain, &prefix)
        }
        (SdblCompletionContext::AfterNestedField { alias, field_chain, prefix }, None) => {
            tracing::warn!(
                alias = %alias,
                field_chain_len = field_chain.len(),
                prefix = %prefix,
                "completion context: AfterNestedField but no scope available"
            );
            CompleteKeywordsUseCase::execute(&prefix)
        }

        (SdblCompletionContext::AfterAsKeyword { context: as_context, suggestion }, _) => {
            tracing::info!(
                ?as_context,
                suggestion = ?suggestion,
                "completion context: AfterAsKeyword"
            );
            CompleteAliasesUseCase::execute_alias_suggestion(suggestion)
        }

        (SdblCompletionContext::JoinTypeKeyword { prefix }, _) => {
            tracing::info!(prefix = %prefix, "completion context: JoinTypeKeyword");
            CompleteJoinTypesUseCase::execute(&prefix)
        }

        (SdblCompletionContext::AfterOnKeyword { prefix }, Some(scope)) => {
            tracing::info!(
                prefix = %prefix,
                "completion context: AfterOnKeyword (with scope)"
            );
            CompleteAliasesUseCase::execute_table_aliases(scope, &prefix)
        }
        (SdblCompletionContext::AfterOnKeyword { prefix }, None) => {
            tracing::warn!(
                prefix = %prefix,
                "completion context: AfterOnKeyword but no scope available"
            );
            CompleteKeywordsUseCase::execute(&prefix)
        }

        (SdblCompletionContext::AfterFromKeyword, _) => {
            tracing::info!("completion context: AfterFromKeyword");
            CompleteMdoUseCase::execute_types("")
        }

        (SdblCompletionContext::InsideMdoType { mdo_type, prefix }, _) => {
            tracing::info!(
                ?mdo_type,
                prefix = %prefix,
                "completion context: InsideMdoType"
            );
            CompleteMdoUseCase::execute_objects(&metadata_provider, mdo_type, &prefix)
        }

        (SdblCompletionContext::AfterMdoObject { mdo_type, object_name, prefix }, _) => {
            tracing::info!(
                ?mdo_type,
                object_name = %object_name,
                prefix = %prefix,
                "completion context: AfterMdoObject"
            );
            CompleteNestedElementsUseCase::execute(
                &metadata_provider,
                mdo_type,
                &object_name,
                &prefix,
            )
        }

        (SdblCompletionContext::InsideValueFunction, _) => {
            tracing::info!("completion context: InsideValueFunction");
            CompleteMdoUseCase::execute_types("")
        }

        (SdblCompletionContext::InsideValueMdoType { mdo_type, prefix, is_russian }, _) => {
            tracing::info!(
                ?mdo_type,
                prefix = %prefix,
                is_russian,
                "completion context: InsideValueMdoType"
            );
            CompleteMdoUseCase::execute_objects(&metadata_provider, mdo_type, &prefix)
        }

        (
            SdblCompletionContext::InsideValueMdoObject {
                mdo_type,
                object_name,
                prefix,
                is_russian,
            },
            _,
        ) => {
            tracing::info!(
                ?mdo_type,
                object_name = %object_name,
                prefix = %prefix,
                is_russian,
                "completion context: InsideValueMdoObject"
            );
            CompleteValueElementsUseCase::execute(
                &metadata_provider,
                mdo_type,
                &object_name,
                &prefix,
                is_russian,
            )
        }

        (
            SdblCompletionContext::AfterCastExpression {
                mdo_type,
                object_name,
                field_chain,
                prefix,
            },
            scope_opt,
        ) => {
            tracing::info!(
                ?mdo_type,
                object_name = %object_name,
                field_chain_len = field_chain.len(),
                prefix = %prefix,
                has_scope = scope_opt.is_some(),
                "completion context: AfterCastExpression"
            );
            CompleteCastFieldsUseCase::execute(
                scope_opt,
                &metadata_provider,
                mdo_type,
                &object_name,
                &field_chain,
                &prefix,
            )
        }

        (SdblCompletionContext::SdblKeywords { prefix }, _) => {
            tracing::info!(prefix = %prefix, "completion context: SdblKeywords");
            CompleteKeywordsUseCase::execute(&prefix)
        }

        (SdblCompletionContext::None, _) => {
            tracing::info!("no completion context detected");
            return None;
        }
    };

    Some(items.into_iter().map(|item| item.into_completion_item()).collect())
}
