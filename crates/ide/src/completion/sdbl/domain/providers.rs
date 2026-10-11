use bsl_metadata::{Configuration, MdoType, MetadataObject, Register};
use sdbl_hir::Scope;
use std::sync::Arc;
use syntax::TextSize;
use vfs::FileId;

pub trait MetadataProvider {
    fn get_configuration(&self) -> Option<Arc<Configuration>>;

    /// Resolve a single metadata object by kind and name. The default reads it
    /// from the whole configuration; the db-backed provider overrides it with the
    /// per-MDO accessor so a completion depends only on the referenced object.
    fn resolve_metadata_object(
        &self,
        mdo_type: MdoType,
        name: &str,
    ) -> Option<Arc<MetadataObject>> {
        self.get_configuration()?.find_metadata_object(mdo_type, name).cloned().map(Arc::new)
    }

    /// Resolve a single register by kind and name. Default reads the whole config;
    /// the db-backed provider overrides it with the per-MDO accessor.
    fn resolve_register(&self, mdo_type: MdoType, name: &str) -> Option<Arc<Register>> {
        self.get_configuration()?
            .find_register_by_type_and_name(mdo_type, name)
            .cloned()
            .map(Arc::new)
    }
}

pub trait ScopeProvider {
    fn get_scope<'a>(
        &self,
        file_id: FileId,
        bsl_literal_range: syntax::TextRange,
        sdbl_offset: TextSize,
        resolver: &'a dyn bsl_metadata::QueryMetadataResolver,
    ) -> Option<Scope<'a>>;
}
