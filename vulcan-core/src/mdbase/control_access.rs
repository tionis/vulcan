use super::MdbaseSchemaCompileError;
use crate::permissions::PermissionFilter;
use std::cell::{Cell, RefCell};
use std::path::Path;

/// A load-local authorization ceiling. A schema denial must abort the registry
/// load, never become an ordinary invalid-type diagnostic that callers can omit.
pub(super) struct ControlAccess<'a> {
    filter: Option<&'a PermissionFilter>,
    denied: Cell<bool>,
    snapshot: RefCell<super::control_snapshot::ControlSnapshot>,
}

impl<'a> ControlAccess<'a> {
    pub(super) fn new(filter: Option<&'a PermissionFilter>) -> Self {
        Self {
            filter,
            denied: Cell::new(false),
            snapshot: RefCell::new(super::control_snapshot::ControlSnapshot::default()),
        }
    }

    // Both checks only ever see control paths: control folders, type,
    // contract and schema files, and the root control files.
    pub(super) fn folder_allowed(&self, folder: &str) -> bool {
        self.filter
            .is_none_or(|filter| filter.admits_control_folder(folder))
    }

    pub(super) fn path_allowed(&self, path: &str) -> bool {
        self.filter
            .is_none_or(|filter| filter.admits_control_path(path))
    }

    pub(super) fn schema(&self, path: &Path) -> Result<(), MdbaseSchemaCompileError> {
        let path = path.to_string_lossy().replace('\\', "/");
        if self.path_allowed(&path) {
            return Ok(());
        }
        self.denied.set(true);
        Err(MdbaseSchemaCompileError::with_code(
            "permission_denied",
            "permission_denied",
        ))
    }

    pub(super) fn denied(&self) -> bool {
        self.denied.get()
    }

    pub(super) fn observe(&self, path: &Path, bytes: Option<&[u8]>) {
        self.snapshot.borrow_mut().observe(path, bytes);
    }

    pub(super) fn observe_schema(
        &self,
        path: &Path,
        observation: super::SchemaReadObservation<'_>,
    ) {
        match observation {
            super::SchemaReadObservation::Present(bytes) => self.observe(path, Some(bytes)),
            super::SchemaReadObservation::Missing => self.observe(path, None),
            super::SchemaReadObservation::Unavailable => {
                self.snapshot.borrow_mut().conflicted = true;
            }
        }
    }

    pub(super) fn control(&self, path: &Path, bytes: &[u8]) {
        self.observe(path, Some(bytes));
        self.snapshot
            .borrow_mut()
            .controls
            .insert(path.to_path_buf());
    }

    pub(super) fn snapshot(&self) -> super::control_snapshot::ControlSnapshot {
        self.snapshot.borrow().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::{PathPermission, ResourceSpecifier};

    #[test]
    fn coverage_is_namespace_based_and_schema_denials_cannot_be_cleared() {
        let filter = PermissionFilter::new(PathPermission {
            allow: vec![
                ResourceSpecifier::Folder("_types/**".into()),
                ResourceSpecifier::Note("schema.yaml".into()),
            ],
            deny: vec![ResourceSpecifier::Note("_types/hidden.md".into())],
        });
        let access = ControlAccess::new(Some(&filter));
        assert!(!access.folder_allowed("_types"));
        assert!(!access.folder_allowed("_contracts"));
        assert!(access.path_allowed("_types/task.md"));
        assert!(!access.denied());
        assert_eq!(
            access.schema(Path::new("hidden.yaml")).unwrap_err().code,
            "permission_denied"
        );
        access.schema(Path::new("schema.yaml")).unwrap();
        assert!(access.denied());
        let unrestricted = ControlAccess::new(None);
        assert!(unrestricted.folder_allowed("_types"));
        unrestricted.schema(Path::new("hidden.yaml")).unwrap();
        assert!(!unrestricted.denied());
    }

    #[test]
    fn implicit_control_reads_admit_every_control_path() {
        let filter = PermissionFilter::new(PathPermission {
            allow: vec![ResourceSpecifier::Folder("Worlds/**".into())],
            deny: Vec::new(),
        });
        let scoped = ControlAccess::new(Some(&filter));
        assert!(!scoped.folder_allowed("_types"));
        assert!(!scoped.path_allowed("mdbase.yaml"));

        let filter = filter.with_implicit_control_reads();
        let writer = ControlAccess::new(Some(&filter));
        assert!(writer.folder_allowed("_types"));
        assert!(writer.folder_allowed("_contracts"));
        assert!(writer.path_allowed("mdbase.yaml"));
        assert!(writer.path_allowed("_types/task.md"));
        writer.schema(Path::new("schemas/task.json")).unwrap();
        assert!(!writer.denied());

        // An explicit path deny still wins; a tag deny does not reach controls.
        let filter = PermissionFilter::new(PathPermission {
            allow: vec![ResourceSpecifier::Folder("Worlds/**".into())],
            deny: vec![
                ResourceSpecifier::Note("_types/hidden.md".into()),
                ResourceSpecifier::Tag("rating/explicit".into()),
            ],
        })
        .with_implicit_control_reads();
        let denied = ControlAccess::new(Some(&filter));
        assert!(!denied.folder_allowed("_types"));
        assert!(!denied.path_allowed("_types/hidden.md"));
        assert!(denied.path_allowed("_types/task.md"));
        assert!(denied.folder_allowed("_contracts"));
        assert!(denied.path_allowed("mdbase.yaml"));
    }
}
