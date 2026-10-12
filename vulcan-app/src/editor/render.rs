//! Evaluated query results for display next to the source.
//!
//! Editors show the results of inline expressions, Dataview and Tasks
//! blocks, and `.base` embeds as virtual text beside the source rather than
//! replacing it. `DataviewJS` is the one region kind that runs code. Its
//! strict `static` sandbox (computation and cache reads; no file access,
//! network, writes, or host execution) is safe for any vault, so it is the
//! floor. Full evaluation (`on`, with the vault's configured sandbox tier and
//! still never writing) follows `vulcan trust`: a trusted vault gets it by
//! default, and an untrusted one never does.

use super::{EditorRange, EditorSession};
use crate::AppError;
use vulcan_core::editor::LineIndex;
use vulcan_core::{
    DataviewJsRenderPolicy, EvaluatedMarkdownOptions, EvaluatedMarkdownRenderer, PermissionGuard,
    RenderDiagnostic,
};

/// One evaluated region of a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorRenderRegion {
    pub range: EditorRange,
    /// `inline` or `block`.
    pub kind: &'static str,
    /// `inline`, `dataview`, `dataviewjs`, `tasks`, `base`, or `notice`.
    pub source: &'static str,
    pub markdown: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorRender {
    pub regions: Vec<EditorRenderRegion>,
    pub diagnostics: Vec<RenderDiagnostic>,
    /// The `DataviewJS` policy the render actually used.
    pub dataview_js: DataviewJsRenderPolicy,
}

impl EditorSession {
    /// Evaluate the dynamic regions of `path` (the open buffer, else the file)
    /// that intersect `range`, or all of them.
    pub fn render(
        &self,
        path: &str,
        range: Option<EditorRange>,
        requested: Option<DataviewJsRenderPolicy>,
    ) -> Result<Option<EditorRender>, AppError> {
        let Some(view) = self.view(path) else {
            return Ok(None);
        };
        if let Some(guard) = self.guard() {
            guard
                .check_read_path(path)
                .map_err(|error| AppError::operation_with_code("permission_denied", error))?;
        }
        let dataview_js = vulcan_core::vault_dataview_js_policy(self.paths(), requested);
        let guard = self.guard().map(|guard| guard as &dyn PermissionGuard);
        let renderer = EvaluatedMarkdownRenderer::load(self.paths(), guard);
        let evaluated = renderer.render_regions(
            &view.text,
            &EvaluatedMarkdownOptions {
                source_path: Some(path),
                dataview_js,
                guard,
                permission_profile: self.permission_profile(),
            },
        );
        let wanted = range.map(|range| {
            let index = LineIndex::new(&view.text);
            index.offset(&view.text, range.start, self.encoding())
                ..index.offset(&view.text, range.end, self.encoding())
        });
        let regions = evaluated
            .regions
            .into_iter()
            .filter(|region| {
                wanted
                    .as_ref()
                    .is_none_or(|wanted| region.start <= wanted.end && wanted.start <= region.end)
            })
            .map(|region| EditorRenderRegion {
                range: self.range(&view.text, &view.line_index, region.start..region.end),
                kind: region.kind,
                source: region.source,
                markdown: region.markdown,
            })
            .collect();
        Ok(Some(EditorRender {
            regions,
            diagnostics: evaluated.diagnostics,
            dataview_js,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;
    use vulcan_core::editor::TextPosition;
    use DataviewJsRenderPolicy::{Off, On, Static};

    const HOME: &str = "---\nstatus: draft\n---\nStatus: `= this.status`\n\n```dataviewjs\ndv.paragraph(\"js ran\")\n```\n\n```tasks\nnot done\n```\n";

    #[test]
    fn renders_dataview_js_by_default_and_fully_once_trusted() {
        let _lock = crate::trust::test_env_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let config_home = tempfile::tempdir().unwrap();
        let previous = std::env::var_os("XDG_CONFIG_HOME");
        std::env::set_var("XDG_CONFIG_HOME", config_home.path());

        let (_dir, mut session) = vault(&[("Home.md", HOME), ("Todo.md", "- [ ] Open item\n")]);
        session.open("Home.md", 1, HOME.to_string());
        let untrusted = session.render("Home.md", None, None).unwrap().unwrap();
        assert_eq!(untrusted.dataview_js, Static);
        crate::trust::add_trust(session.paths().vault_root()).unwrap();
        let trusted = session.render("Home.md", None, None).unwrap().unwrap();
        assert_eq!(trusted.dataview_js, On);

        match previous {
            Some(value) => std::env::set_var("XDG_CONFIG_HOME", value),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
        for render in [untrusted, trusted] {
            let js = render
                .regions
                .iter()
                .find(|region| region.source == "dataviewjs")
                .expect("the dataviewjs block is evaluated");
            assert!(js.markdown.contains("js ran"), "{}", js.markdown);
        }
    }

    #[test]
    fn renders_regions_of_open_buffers_within_a_range() {
        let (_dir, mut session) = vault(&[("Home.md", HOME), ("Todo.md", "- [ ] Open item\n")]);
        session.open("Home.md", 1, HOME.to_string());
        let render = session.render("Home.md", None, Some(Off)).unwrap().unwrap();
        assert_eq!(render.dataview_js, Off);
        let sources = render
            .regions
            .iter()
            .map(|region| region.source)
            .collect::<Vec<_>>();
        assert_eq!(sources, vec!["inline", "notice", "tasks"]);
        assert_eq!(render.regions[0].markdown, "draft");
        assert_eq!(
            render.regions[0].range.start,
            position_of(HOME, "`= this", 0)
        );
        assert!(render.regions[2].markdown.contains("Open item"));

        let tasks_line = position_of(HOME, "```tasks", 0);
        let only_tasks = session
            .render(
                "Home.md",
                Some(EditorRange {
                    start: tasks_line,
                    end: TextPosition {
                        line: tasks_line.line,
                        column: 3,
                    },
                }),
                Some(Off),
            )
            .unwrap()
            .unwrap();
        assert_eq!(only_tasks.regions.len(), 1);
        assert_eq!(only_tasks.regions[0].source, "tasks");
        assert_eq!(session.render("Missing.md", None, Some(Off)).unwrap(), None);
    }
}
